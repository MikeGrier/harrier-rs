// Copyright (c) 2026, Michael Grier

//! `Source` builder and `SourceError` — the entry point for opening a
//! byte stream for processing by weaver.

use std::sync::Arc;

use encoding_rs::Encoding;
use redwing::Branch;

use crate::encoding::{
    detect_bom, detect_line_ending, BomPolicy, ChardetngDetector, EncodingDetector, LineEnding,
    SourceConfig,
};

// ── MA-11: SourceError ────────────────────────────────────────────────────────

/// Errors that can occur while opening a [`Source`].
#[derive(Debug)]
pub enum SourceError {
    /// An I/O error occurred while reading the probe bytes from the branch.
    Io(std::io::Error),

    /// The detected (or hinted) encoding cannot encode text back to bytes —
    /// i.e. it is a decode-only encoding in the WHATWG Encoding Standard.
    /// This is detected at open time so that callers receive a clear error
    /// rather than a panic or silent failure during a later write.
    ///
    /// The `encoding_name` field carries the WHATWG name of the offending
    /// encoding for diagnostic messages.
    EncodeDecodeAsymmetry { encoding_name: &'static str },

    /// Heuristic encoding detection ran on the probe bytes but could not
    /// produce a usable result.  This variant is reserved for future use;
    /// `chardetng` always returns *some* encoding, so callers should treat
    /// this as an internal error if they encounter it.
    DetectionFailure,
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::Io(e) => write!(f, "I/O error reading source probe: {e}"),
            SourceError::EncodeDecodeAsymmetry { encoding_name } => write!(
                f,
                "encoding '{encoding_name}' cannot encode text (decode-only encoding)"
            ),
            SourceError::DetectionFailure => {
                write!(f, "encoding detection failed to produce a usable result")
            }
        }
    }
}

impl std::error::Error for SourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SourceError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SourceError {
    fn from(e: std::io::Error) -> Self {
        SourceError::Io(e)
    }
}

// ── MA-12: Source ─────────────────────────────────────────────────────────────

/// The opened, probed state of a byte-stream branch, ready to be converted
/// into one of `Chars`, `Lines`, or `Buffer`.
///
/// `Source` is created by [`Source::new`], which:
///
/// 1. Reads a configurable probe prefix of the branch.
/// 2. Detects a BOM (if present and `bom_policy` is `Honour`).
/// 3. Runs `chardetng` heuristic detection when no BOM is present and no
///    encoding hint was supplied.
/// 4. Validates that the selected encoding supports both decode and encode
///    (i.e. is not a decode-only WHATWG encoding).
/// 5. Runs a majority-vote line-ending detector over the probe bytes.
///
/// All of these steps happen once at open time so that the downstream types
/// (`Chars`, `Lines`, `Buffer`) receive a fully-resolved encoding and line-
/// ending policy without repeating the probe work.
pub struct Source {
    /// The underlying byte-stream.
    branch: Arc<dyn Branch>,
    /// The WHATWG encoding selected for this source.
    encoding: &'static Encoding,
    /// Number of BOM bytes at the start of the branch to skip when reading
    /// content.  Zero when no BOM was present or when `bom_policy` was
    /// `Ignore`.
    bom_len: usize,
    /// The dominant line-ending convention detected in the probe.
    line_ending: LineEnding,
}

impl Source {
    /// Open a branch and probe it to determine its encoding and line-ending
    /// convention.
    ///
    /// `config` controls how the probe is performed; see [`SourceConfig`]
    /// for field-level documentation.
    ///
    /// # Errors
    ///
    /// - [`SourceError::Io`] — any I/O error reading probe bytes.
    /// - [`SourceError::EncodeDecodeAsymmetry`] — the selected encoding is
    ///   decode-only (cannot round-trip text back to bytes).
    pub fn new(branch: Arc<dyn Branch>, config: SourceConfig) -> Result<Self, SourceError> {
        // Clamp probe_len to at least 3 bytes (minimum BOM length).
        let probe_len = config.probe_len.max(3);

        // Read the probe prefix.
        let available = branch.byte_len().min(probe_len as u64) as usize;
        let mut probe = vec![0u8; available];
        if available > 0 {
            branch.read_at(0, &mut probe)?;
        }

        // ── Step 1: BOM detection ──────────────────────────────────────────
        let (encoding, bom_len) = if config.bom_policy == BomPolicy::Honour {
            let bom = detect_bom(&probe);
            if let Some(enc) = bom.encoding {
                (enc, bom.bom_len)
            } else {
                (resolve_encoding_no_bom(&probe, &config)?, 0)
            }
        } else {
            // BomPolicy::Ignore — treat BOM bytes as content, skip BOM sniff.
            (resolve_encoding_no_bom(&probe, &config)?, 0)
        };

        // ── Step 2: Encode/decode symmetry check ──────────────────────────
        // encoding_rs exposes whether an encoder exists via `new_encoder()`.
        // The method is not available on `Encoding` directly; instead we
        // attempt to create an encoder and check that it is not the error
        // encoder.  The standard way is to call `can_encode_everything()` or
        // simply try `encoding.new_encoder()` — all WHATWG encodings that
        // are encodable return an encoder; decode-only ones return the
        // replacement encoder which encodes nothing useful.
        //
        // encoding_rs documents that `Encoding::for_label` returns `None`
        // for labels of decode-only encodings when used for output; the
        // symmetrical runtime check is to verify that the encoder produced
        // is not the replacement encoder, i.e. that it can actually encode
        // the range of characters we care about.
        //
        // The simplest reliable check: encode a known ASCII character. If
        // the encoding can handle it (all WHATWG encodable encodings can),
        // the encoder is usable.  Decode-only encodings (replacement, x-
        // user-defined) will fail or substitute.
        validate_encoder(encoding)?;

        // ── Step 3: Line-ending detection ─────────────────────────────────
        // Scan the probe bytes after the BOM.  For multi-byte encodings
        // (notably UTF-16LE/BE) a `\r\n` sequence is encoded as four bytes
        // with interleaved zero bytes, so running the detector directly on
        // the raw probe would see bare `\r` and `\n` bytes and never
        // classify the source as `CrLf`.  To handle this correctly we
        // decode the probe to UTF-8 first when the selected encoding is
        // anything other than a single-byte / ASCII-superset encoding
        // (i.e. when `is_ascii_compatible()` is false, which covers
        // UTF-16LE, UTF-16BE, and the like), and then run the detector
        // over the decoded UTF-8 bytes.  For ASCII-compatible encodings
        // (UTF-8, windows-1252, the ISO-8859 family, EUC-*, Shift_JIS,
        // GB18030, Big5, etc.) `\r` and `\n` retain their ASCII byte
        // values and the raw-byte scan is correct and cheaper.
        let content_probe = if bom_len < probe.len() {
            &probe[bom_len..]
        } else {
            &[]
        };

        let decoded_storage: String;
        let detect_bytes: &[u8] = if encoding.is_ascii_compatible() {
            content_probe
        } else {
            // Decode without BOM handling (we've already stripped it) and
            // with malformed sequences replaced; we only need accurate
            // CR/LF positions, not a perfect round-trip.
            let mut decoder = encoding.new_decoder_without_bom_handling();
            let max_len = decoder
                .max_utf8_buffer_length_without_replacement(content_probe.len())
                .unwrap_or(content_probe.len() * 4);
            decoded_storage = {
                let mut s = String::with_capacity(max_len);
                let _ = decoder.decode_to_string(content_probe, &mut s, true);
                s
            };
            decoded_storage.as_bytes()
        };

        let line_ending = detect_line_ending(detect_bytes, config.line_ending_default)
            // If no terminators were found, fall back to the caller's default
            // or LF as the universal fallback.
            .or(config.line_ending_default)
            .unwrap_or(LineEnding::Lf);

        Ok(Source {
            branch,
            encoding,
            bom_len,
            line_ending,
        })
    }

    /// The branch this source was opened from.
    pub fn branch(&self) -> Arc<dyn Branch> {
        self.branch.clone()
    }

    /// The WHATWG encoding detected (or supplied) for this source.
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// The dominant line-ending convention detected in the probe prefix.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// The number of BOM bytes at the start of the branch that should be
    /// skipped when reading decoded content.  Zero when no BOM was present
    /// or when `bom_policy` was [`BomPolicy::Ignore`].
    pub fn bom_len(&self) -> usize {
        self.bom_len
    }

    /// Convert this `Source` into a [`Chars`] value for encoding-only,
    /// character-stream–oriented access.
    ///
    /// Consumes `self`.  The `Source` is moved into the returned [`Chars`];
    /// the branch reference, encoding, BOM length, and line-ending policy are
    /// all preserved without any additional probing.
    ///
    /// # Errors
    ///
    /// Currently infallible; the `Result` return type is present to allow
    /// future error conditions (e.g. I/O failures while seeding the DBCS
    /// sync-point index) without a breaking API change.
    pub fn as_chars(self) -> Result<crate::chars::Chars, crate::chars::CharsError> {
        Ok(crate::chars::Chars::from_source(self))
    }

    /// Convert this `Source` into a [`Lines`](crate::lines::Lines) value for
    /// forward line-oriented iteration and arbitrary-span materialisation.
    ///
    /// Consumes `self`.  The branch reference, encoding, BOM length, and
    /// line-ending policy are all preserved without any additional probing.
    ///
    /// # Errors
    ///
    /// Currently infallible; the `Result` return type is present to allow
    /// future error conditions without a breaking API change.
    pub fn as_lines(self) -> Result<crate::lines::Lines, crate::lines::LinesError> {
        Ok(crate::lines::Lines::from_source(self))
    }

    /// Convert this `Source` into a [`Buffer`](crate::buffer::Buffer) for
    /// random-access, line-map–backed editor operations.
    ///
    /// Consumes `self`.  The branch reference, encoding, BOM length, and
    /// line-ending policy are all preserved.  The internal [`LineMap`] starts
    /// fully unscanned; segments are scanned on demand as `Buffer` operations
    /// require line-number information.
    ///
    /// To receive [`LineMapEvent`] notifications, call
    /// [`Buffer::with_sender`](crate::buffer::Buffer::with_sender) on the
    /// returned value.
    ///
    /// # Errors
    ///
    /// Currently infallible; the `Result` return type is present to allow
    /// future error conditions without a breaking API change.
    pub fn as_buffer(self) -> Result<crate::buffer::Buffer, crate::buffer::BufferError> {
        crate::buffer::Buffer::from_source(self)
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Resolve the encoding when no BOM was found (or BOM was ignored).
/// Uses the caller's hint if provided, otherwise runs `chardetng`.
fn resolve_encoding_no_bom(
    probe: &[u8],
    config: &SourceConfig,
) -> Result<&'static Encoding, SourceError> {
    if let Some(hint) = config.encoding_hint {
        return Ok(hint);
    }

    // No hint — run the heuristic detector.
    let mut detector = ChardetngDetector::new();
    detector.feed(probe, true);
    let allow_utf8 = true; // Always allow UTF-8 from heuristic detection.
    let enc = detector.guess(allow_utf8);
    // chardetng always returns a valid encoding; `DetectionFailure` is
    // reserved for future implementations that may genuinely fail.
    if enc.name().is_empty() {
        return Err(SourceError::DetectionFailure);
    }
    Ok(enc)
}

/// Verify that `encoding` is not a WHATWG decode-only encoding.
///
/// The WHATWG Encoding Standard designates "replacement" as the sole
/// decode-only encoding.  In `encoding_rs` its `new_encoder()` delegates
/// internally to the UTF-8 encoder, so a test-encode approach produces a
/// misleading `Ok` result.  We therefore check by pointer identity against
/// `encoding_rs::REPLACEMENT`, which is the canonical `&'static Encoding`
/// value for that encoding.
///
/// Note: `encoding_rs` also lacks real encoders for UTF-16LE and UTF-16BE
/// (those are decode-only in WHATWG output-encoding terms).  Those encodings
/// are valid for read-only access via [`Lines`](crate::lines::Lines) and
/// [`Chars`](crate::chars::Chars), but are rejected by
/// [`Buffer::from_source`](crate::buffer::Buffer) because edits would
/// silently emit UTF-8 bytes instead of preserving the UTF-16 encoding.
///
/// Changing this check is a breaking change because callers rely on
/// `Source::new` rejecting decode-only encodings at open time.
fn validate_encoder(encoding: &'static Encoding) -> Result<(), SourceError> {
    if std::ptr::eq(encoding, encoding_rs::REPLACEMENT) {
        return Err(SourceError::EncodeDecodeAsymmetry {
            encoding_name: encoding.name(),
        });
    }
    Ok(())
}
