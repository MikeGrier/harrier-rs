// Copyright (c) 2026, Michael Grier

//! `DenormaliseWriter` — a `Write` adapter that re-inserts original line
//! terminators into normalised (LF-only) replacement text.
//!
//! # Overview
//!
//! When a sed-style replacement is applied to a normalised view of a source
//! document, the replacement text is LF-only.  Before writing it to the
//! output, each LF must be replaced by whichever line terminator the
//! *original* line used.  The original terminators are supplied as an
//! [`Iterator<Item = LineEnding>`] built from the terminator log recorded
//! during the forward scan.
//!
//! # M-vs-N terminator preservation rule
//!
//! Let **M** = number of original line terminators consumed by the replaced
//! region, and **N** = number of `\n` bytes in the replacement text.
//!
//! | Relation | Behaviour |
//! |---|---|
//! | M == N | Each replacement `\n` is substituted with the next terminator from `I`. |
//! | M < N  | The first M replacement `\n`s are substituted from `I`; subsequent `\n`s are written as plain `\n`. |
//! | M > N  | All N replacement `\n`s are substituted from `I`; the remaining (M − N) terminators still in `I` are emitted via [`DenormaliseWriter::finish`]. |
//!
//! Callers **must** call [`DenormaliseWriter::finish`] after the last
//! `write` call to ensure that any surplus terminators (M > N case) are
//! flushed to the underlying writer.
//!
//! # Write-logic
//!
//! See [`DenormaliseWriter`]'s [`std::io::Write`] implementation (MA-31).

use std::io::{self, Write};

use encoding_rs::Encoding;

use crate::encoding::LineEnding;

// ── helpers ───────────────────────────────────────────────────────────────────

/// The raw bytes of `le` in `encoding`'s code units.
///
/// This is the single source of truth for terminator *bytes* across harrier.
/// Single-byte and UTF-8 encodings use one byte per unit; UTF-16LE/BE use two,
/// so e.g. a CRLF terminator is four bytes there
/// (`[0x0D,0x00,0x0A,0x00]` for LE). Byte order follows the encoding.
///
/// Changing the units emitted here is a breaking change.
pub(crate) fn line_ending_bytes(le: LineEnding, encoding: &'static Encoding) -> Vec<u8> {
    // The terminator as ASCII code points; widened to code units below.
    let ascii: &[u8] = match le {
        LineEnding::Lf => b"\n",
        LineEnding::CrLf => b"\r\n",
        LineEnding::Cr => b"\r",
    };
    if encoding == encoding_rs::UTF_16LE {
        ascii.iter().flat_map(|&b| [b, 0x00]).collect()
    } else if encoding == encoding_rs::UTF_16BE {
        ascii.iter().flat_map(|&b| [0x00, b]).collect()
    } else {
        ascii.to_vec()
    }
}

/// Write the byte sequence that represents `le` in `encoding` to `w`.
///
/// Delegates to [`line_ending_bytes`] so single-byte and UTF-16 terminators are
/// produced from one place.
pub(crate) fn write_line_ending(
    w: &mut impl Write,
    le: LineEnding,
    encoding: &'static Encoding,
) -> io::Result<()> {
    w.write_all(&line_ending_bytes(le, encoding))
}

/// Whether `encoding` is a two-byte-per-unit UTF-16 encoding; `Some(true)` for
/// little-endian, `Some(false)` for big-endian, `None` for single-byte / UTF-8.
fn utf16_le(encoding: &'static Encoding) -> Option<bool> {
    if encoding == encoding_rs::UTF_16LE {
        Some(true)
    } else if encoding == encoding_rs::UTF_16BE {
        Some(false)
    } else {
        None
    }
}

// ── MA-30: DenormaliseWriter struct ──────────────────────────────────────────

/// A `Write` adapter that re-inserts original line terminators into normalised
/// (LF-only) replacement text.
///
/// Wrap an output writer and a terminator iterator, then write the normalised
/// replacement bytes through this adapter.  Each `\n` byte in the input is
/// substituted with the next terminator from `I`; if `I` is exhausted the
/// `\n` is written verbatim.  Non-`\n` bytes are passed through unchanged.
///
/// After all replacement bytes have been written, call [`finish`] to emit any
/// surplus terminators that remain in `I` (the M > N case).
///
/// [`finish`]: DenormaliseWriter::finish
pub struct DenormaliseWriter<W: Write, I: Iterator<Item = LineEnding>> {
    /// The underlying output stream.
    inner: W,
    /// Iterator of original line terminators from the terminator log.
    terminators: I,
    /// The output encoding, so LF *markers* are recognized and terminators
    /// emitted as code units (one byte for single-byte/UTF-8, two for UTF-16).
    encoding: &'static Encoding,
    /// UTF-16 only: a single leftover byte when a `write` ended mid-code-unit,
    /// completed by the first byte of the next `write`.
    pending: Option<u8>,
}

impl<W: Write, I: Iterator<Item = LineEnding>> DenormaliseWriter<W, I> {
    /// Create a `DenormaliseWriter` wrapping `inner` and drawing original
    /// terminators from `terminators`, assuming UTF-8 / single-byte source
    /// encoding.
    ///
    /// `terminators` should yield exactly M items, where M is the number of
    /// line terminators in the original source region that was replaced.
    ///
    /// This is a UTF-8-defaulting shim kept for source compatibility; use
    /// [`new_with_encoding`](DenormaliseWriter::new_with_encoding) for UTF-16
    /// sources, where terminators must be emitted as 2-byte code units.
    pub fn new(inner: W, terminators: I) -> Self {
        Self::new_with_encoding(inner, terminators, encoding_rs::UTF_8)
    }

    /// Create a `DenormaliseWriter` wrapping `inner` and drawing original
    /// terminators from `terminators`, emitting in `encoding`'s code units.
    ///
    /// `terminators` should yield exactly M items, where M is the number of
    /// line terminators in the original source region that was replaced.
    pub fn new_with_encoding(inner: W, terminators: I, encoding: &'static Encoding) -> Self {
        DenormaliseWriter {
            inner,
            terminators,
            encoding,
            pending: None,
        }
    }

    /// Emit any terminators remaining in `I` (the M > N case) and return the
    /// underlying writer.
    ///
    /// Must be called after the last `write` call.  If M ≤ N no surplus
    /// terminators remain and this is a no-op beyond returning `inner`.
    ///
    /// # Errors
    ///
    /// Returns the first [`std::io::Error`] encountered while writing a
    /// surplus terminator.  The inner writer is consumed regardless; any
    /// partially-written output is not rolled back.
    pub fn finish(mut self) -> io::Result<W> {
        // A leftover half code unit is malformed input; emit it rather than
        // silently drop it.
        if let Some(b) = self.pending.take() {
            self.inner.write_all(&[b])?;
        }
        for le in self.terminators.by_ref() {
            write_line_ending(&mut self.inner, le, self.encoding)?;
        }
        Ok(self.inner)
    }

    /// Consume the writer and return the underlying `W` *without* flushing
    /// surplus terminators.
    ///
    /// Prefer [`finish`] in almost all cases.  Use this only when you are
    /// certain M ≤ N and no surplus terminators exist, or when you are
    /// intentionally discarding them.
    ///
    /// [`finish`]: DenormaliseWriter::finish
    pub fn into_inner(self) -> W {
        self.inner
    }
}

// ── MA-31: Write impl ─────────────────────────────────────────────────────────

impl<W: Write, I: Iterator<Item = LineEnding>> Write for DenormaliseWriter<W, I> {
    /// Write `buf`, substituting each LF *marker* with the next terminator from
    /// `I` (as code units in the output encoding), passing everything else
    /// through.
    ///
    /// ## M-vs-N terminator preservation rule
    ///
    /// - If `I` still has items when an LF marker is encountered, it is replaced
    ///   by the next terminator from `I` (which may itself be an LF, a CRLF, or
    ///   a CR, in the encoding's code units).
    /// - If `I` is exhausted (M < N case), remaining LF markers are written
    ///   verbatim.
    ///
    /// For single-byte / UTF-8 output the LF marker is the byte `0x0A`. For
    /// UTF-16 it is the LF *code unit* (`[0x0A,0x00]` LE, `[0x00,0x0A]` BE);
    /// scanning is 2-byte aligned, and a `write` that ends mid-unit buffers the
    /// half unit until the next `write` (see [`pending`](Self::pending)).
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match utf16_le(self.encoding) {
            None => self.write_single_byte(buf)?,
            Some(le) => self.write_utf16(buf, le)?,
        }
        Ok(buf.len())
    }

    /// Flush the underlying writer.
    ///
    /// Does **not** emit surplus terminators; call [`finish`] for that.
    ///
    /// A pending half UTF-16 code unit (see [`write`](Self::write)) is
    /// written out verbatim rather than held, since `flush` must not leave
    /// previously-accepted bytes unreachable in the destination. This can
    /// only split a real LF/CR unit's substitution if the caller flushes
    /// between the two `write` calls that supplied its two bytes — an
    /// unusual usage pattern for a text stream.
    ///
    /// [`finish`]: DenormaliseWriter::finish
    fn flush(&mut self) -> io::Result<()> {
        if let Some(b) = self.pending.take() {
            self.inner.write_all(&[b])?;
        }
        self.inner.flush()
    }
}

impl<W: Write, I: Iterator<Item = LineEnding>> DenormaliseWriter<W, I> {
    /// Single-byte / UTF-8 substitution: replace each `0x0A` byte.
    fn write_single_byte(&mut self, buf: &[u8]) -> io::Result<()> {
        let mut i = 0;
        while i < buf.len() {
            match buf[i..].iter().position(|&b| b == b'\n') {
                None => {
                    self.inner.write_all(&buf[i..])?;
                    i = buf.len();
                }
                Some(rel) => {
                    if rel > 0 {
                        self.inner.write_all(&buf[i..i + rel])?;
                    }
                    match self.terminators.next() {
                        Some(le) => write_line_ending(&mut self.inner, le, self.encoding)?,
                        None => self.inner.write_all(b"\n")?,
                    }
                    i += rel + 1;
                }
            }
        }
        Ok(())
    }

    /// UTF-16 substitution: replace each LF *code unit*, 2-byte aligned, with a
    /// leftover half-unit carried across `write` calls in `pending`.
    fn write_utf16(&mut self, buf: &[u8], le: bool) -> io::Result<()> {
        let lf_unit: [u8; 2] = if le { [0x0A, 0x00] } else { [0x00, 0x0A] };
        let mut idx = 0;

        // Complete a half unit left over from the previous write.
        if let Some(first) = self.pending.take() {
            if buf.is_empty() {
                self.pending = Some(first);
                return Ok(());
            }
            self.emit_unit([first, buf[0]], lf_unit)?;
            idx = 1;
        }

        while idx + 2 <= buf.len() {
            self.emit_unit([buf[idx], buf[idx + 1]], lf_unit)?;
            idx += 2;
        }
        if idx < buf.len() {
            self.pending = Some(buf[idx]);
        }
        Ok(())
    }

    /// Emit one UTF-16 code unit, substituting an LF unit with the next
    /// terminator (or passing it through when `I` is exhausted).
    fn emit_unit(&mut self, unit: [u8; 2], lf_unit: [u8; 2]) -> io::Result<()> {
        if unit == lf_unit {
            match self.terminators.next() {
                Some(le) => write_line_ending(&mut self.inner, le, self.encoding)?,
                None => self.inner.write_all(&unit)?,
            }
        } else {
            self.inner.write_all(&unit)?;
        }
        Ok(())
    }
}
