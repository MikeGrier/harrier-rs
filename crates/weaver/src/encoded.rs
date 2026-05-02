// Copyright (c) 2026, Michael Grier

//! The `Encoded` trait and its associated error type.
//!
//! `Encoded` is the common surface shared by [`Chars`], [`Lines`], and
//! [`Buffer`]: each holds an `Arc<dyn Branch>`, knows its WHATWG encoding,
//! and can re-encode a `&str` back to bytes in that encoding.  Generic code
//! that accepts any of the three types uses `Encoded` as the bound.
//!
//! [`Chars`]: crate::chars::Chars
//! [`Lines`]: crate::lines::Lines
//! [`Buffer`]: crate::buffer::Buffer

use std::sync::Arc;

use encoding_rs::Encoding;
use redwing::Branch;

use crate::encoding::LineEnding;

// ── MA-10: EncodeError ────────────────────────────────────────────────────────

/// Error returned by [`Encoded::encode`] when the input string cannot be
/// faithfully represented in the target encoding.
///
/// This occurs when at least one Unicode scalar value in the input has no
/// corresponding code point in the target encoding.  Callers that need the
/// position of the first unmappable character should re-encode characters
/// individually to locate it; `EncodeError` deliberately omits the offending
/// character from its public interface to keep the fast-path simple.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodeError {
    /// At least one character in the input could not be represented in the
    /// named encoding.
    ///
    /// `encoding_name` is the WHATWG name of the encoding (e.g. `"windows-1252"`).
    /// Changing any variant or field is a breaking change because callers may
    /// match on them.
    Unmappable { encoding_name: &'static str },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Unmappable { encoding_name } => write!(
                f,
                "one or more characters cannot be encoded in '{encoding_name}'"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

// ── MA-10: Encoded trait ──────────────────────────────────────────────────────

/// Common surface shared by `Chars`, `Lines`, and `Buffer`.
///
/// Every `Encoded` type holds:
/// - A reference to the underlying byte-stream ([`Branch`]).
/// - The WHATWG encoding used to decode/encode that stream.
/// - The dominant line-ending convention detected when the source was opened.
///
/// The provided [`encode`](Encoded::encode) method encodes a `&str` into the
/// target encoding using `encoding_rs`.  Implementors only need to supply the
/// three required methods; the encoding logic is centralised here.
pub trait Encoded {
    /// The underlying byte-stream branch.
    fn branch(&self) -> Arc<dyn Branch>;

    /// The WHATWG encoding used to decode and re-encode this source.
    ///
    /// The encoding is guaranteed to be a two-way encoding (not decode-only)
    /// at the time the [`Source`](crate::source::Source) was opened.
    fn encoding(&self) -> &'static Encoding;

    /// The dominant line-ending convention as detected during source probing.
    fn line_ending(&self) -> LineEnding;

    /// Encode `text` into the bytes of [`encoding()`](Self::encoding).
    ///
    /// Returns `Ok(bytes)` when every character in `text` has an exact
    /// representation in the target encoding with no substitution.
    ///
    /// Returns [`Err(EncodeError::Unmappable)`] when any character in `text`
    /// cannot be faithfully encoded.
    ///
    /// # Allocation
    ///
    /// Allocates a `Vec<u8>` sized by `encoding_rs`'s pessimistic estimate
    /// for lossless encoding.  No intermediate heap allocations are performed
    /// unless the pre-allocated buffer is unexpectedly too small (which should
    /// not occur under normal circumstances).
    fn encode(&self, text: &str) -> Result<Vec<u8>, EncodeError> {
        encode_with(self.encoding(), text)
    }

    /// Return `Ok(true)` when every character in `text` can be faithfully
    /// represented in [`encoding()`](Self::encoding), or `Ok(false)` when at
    /// least one character has no mapping in the encoding.
    ///
    /// `Err` is returned only when the encoder itself fails unexpectedly (e.g.
    /// an internal buffer overflow); under normal operation with `encoding_rs`
    /// this branch is unreachable, but the signature leaves room for future
    /// error conditions.
    ///
    /// All WHATWG encodings are strict ASCII supersets, so pure-ASCII input is
    /// accepted without invoking the encoder (no allocation).
    fn can_encode(&self, text: &str) -> Result<bool, EncodeError> {
        can_encode_with(self.encoding(), text)
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Encode `text` into `encoding`'s byte representation.
///
/// Extracted as a free function so that it can be unit-tested independently of
/// any concrete implementor of [`Encoded`].
///
/// Uses `encode_from_utf8_without_replacement` so that unmappable characters
/// are surfaced as `EncoderResult::Unmappable` rather than silently emitted
/// as HTML numeric character references.
pub(crate) fn encode_with(encoding: &'static Encoding, text: &str) -> Result<Vec<u8>, EncodeError> {
    let mut encoder = encoding.new_encoder();

    // Pessimistic allocation sized for the input with no unmappable
    // characters.  This is sufficient because we use
    // `encode_from_utf8_without_replacement` which never expands unmappable
    // chars into longer HTML entity sequences.
    let capacity = encoder
        .max_buffer_length_from_utf8_if_no_unmappables(text.len())
        .unwrap_or_else(|| text.len().saturating_mul(4).max(16));

    let mut out = vec![0u8; capacity];
    let (result, _read, written) =
        encoder.encode_from_utf8_without_replacement(text, &mut out, true);

    match result {
        encoding_rs::EncoderResult::InputEmpty => {
            out.truncate(written);
            Ok(out)
        }
        encoding_rs::EncoderResult::Unmappable(_) => Err(EncodeError::Unmappable {
            encoding_name: encoding.name(),
        }),
        encoding_rs::EncoderResult::OutputFull => {
            // The pessimistic allocation should prevent this, but handle it
            // defensively — the caller cannot use this encoding for this text.
            Err(EncodeError::Unmappable {
                encoding_name: encoding.name(),
            })
        }
    }
}

/// Return `Ok(true)` when every character in `text` can be faithfully
/// represented in `encoding`, or `Ok(false)` when at least one character has
/// no mapping in the encoding.
///
/// `Err` is returned only for unexpected encoder failures (e.g. internal
/// buffer overflow); under normal operation with `encoding_rs` this branch is
/// unreachable, but the signature leaves room for future error conditions.
///
/// All WHATWG encodings are ASCII supersets, so pure-ASCII input is accepted
/// without invoking the encoder (no allocation).  Non-ASCII input is validated
/// by attempting a real encode; the output bytes are discarded.
pub(crate) fn can_encode_with(
    encoding: &'static Encoding,
    text: &str,
) -> Result<bool, EncodeError> {
    if text.is_ascii() {
        return Ok(true);
    }
    encode_with(encoding, text)
        .map(|_| true)
        .or_else(|e| match e {
            EncodeError::Unmappable { .. } => Ok(false),
        })
}
