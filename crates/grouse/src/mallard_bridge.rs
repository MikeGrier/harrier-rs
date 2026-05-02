// Copyright (c) 2026, Michael Grier

//! Bridge between grouse and mallard.
//!
//! [`GrouseLineSource`] implements [`mallard::LineSource`] for a grouse
//! [`Lines`] iterator, making it possible to populate a mallard
//! [`LineBuffer`](mallard::LineBuffer) directly from any grouse-opened
//! source without going through an intermediate file.
//!
//! All lines are collected eagerly during construction: each raw byte slice
//! is decoded from the source encoding to UTF-8 and stripped of its
//! line terminator before being stored.  The stored strings are then
//! replayed by [`LineSource::for_each_line`], which requires only a shared
//! reference.
//!
//! [`GrouseEncodingValidator`] implements [`mallard::EncodingValidator`] for
//! any `&'static Encoding` from `encoding_rs`.  It uses grouse's
//! `can_encode_with` to test whether a given line can round-trip through the
//! target encoding, with an ASCII fast-path that avoids any allocation.
//!
//! [`from_grouse`] is a convenience constructor that combines both: it accepts
//! a grouse [`Source`](crate::source::Source), converts it to a
//! [`GrouseLineSource`], installs a [`GrouseEncodingValidator`] as the primary
//! validator, and returns a fully-populated [`mallard::LineBuffer`].

use std::sync::Arc;

use encoding_rs::Encoding;
use mallard::{EncodingError, EncodingValidator, LineBuffer, LineSource};

use crate::{
    encoded::can_encode_with,
    lines::{LineTerminator, Lines},
    source::Source,
};

// ── GrouseLineSource ──────────────────────────────────────────────────────────

/// A [`LineSource`] backed by a grouse [`Lines`] iterator.
///
/// Eagerly consumes the iterator during construction, decoding each line from
/// the source encoding to UTF-8 and stripping its line terminator.  The
/// resulting owned strings are replayed by [`for_each_line`].
///
/// [`for_each_line`]: GrouseLineSource::for_each_line
pub struct GrouseLineSource {
    lines: Vec<String>,
}

impl GrouseLineSource {
    /// Consume `lines`, decode every line from the source encoding to UTF-8,
    /// strip terminators, and store the results.
    ///
    /// Bytes that cannot be decoded in the source encoding are replaced with
    /// U+FFFD (REPLACEMENT CHARACTER) — the `encoding_rs` standard
    /// substitution policy.  This keeps the constructor infallible while
    /// still preserving as much content as possible.
    pub fn new(lines: Lines) -> Self {
        let encoding = lines.encoding();
        let collected: Vec<String> = lines
            .map(|(bytes, terminator)| {
                // Strip the appended LF byte for all terminated lines.
                // LineTerminator::End means there was no trailing newline,
                // so the bytes contain no terminator to strip.
                let content = match terminator {
                    LineTerminator::End => &bytes[..],
                    LineTerminator::Ending(_) => {
                        // The iterator always appends a single '\n' byte.
                        let len = bytes.len();
                        if len > 0 && bytes[len - 1] == b'\n' {
                            &bytes[..len - 1]
                        } else {
                            &bytes[..]
                        }
                    }
                };
                // Decode from source encoding to UTF-8; replace unmappables.
                let (decoded, _enc, _had_errors) = encoding.decode(content);
                decoded.into_owned()
            })
            .collect();
        GrouseLineSource { lines: collected }
    }
}

impl LineSource for GrouseLineSource {
    fn line_count_hint(&self) -> Option<usize> {
        Some(self.lines.len())
    }

    fn for_each_line(&self, f: &mut dyn FnMut(&str)) {
        for line in &self.lines {
            f(line.as_str());
        }
    }
}

// ── GrouseEncodingValidator ───────────────────────────────────────────────────

/// An [`EncodingValidator`] that ensures every line can be faithfully
/// represented in a specific `encoding_rs` encoding.
///
/// Validation calls `can_encode_with(encoding, line)`, which applies an ASCII
/// fast-path (no allocation) and only invokes the full encoder for non-ASCII
/// input.  `Ok(true)` → accept; `Ok(false)` or `Err` → reject with a
/// descriptive [`EncodingError`].
///
/// `GrouseEncodingValidator` is `Send + Sync` because `&'static Encoding` is
/// both.
pub struct GrouseEncodingValidator {
    encoding: &'static Encoding,
}

impl GrouseEncodingValidator {
    /// Create a validator that enforces `encoding`.
    pub fn new(encoding: &'static Encoding) -> Self {
        GrouseEncodingValidator { encoding }
    }

    /// The encoding this validator enforces.
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }
}

impl EncodingValidator for GrouseEncodingValidator {
    fn validate(&self, line: &str) -> Result<(), EncodingError> {
        match can_encode_with(self.encoding, line) {
            Ok(true) => Ok(()),
            Ok(false) => Err(EncodingError {
                line: line.into(),
                description: format!(
                    "line contains characters that cannot be encoded in '{}'",
                    self.encoding.name()
                )
                .into_boxed_str(),
            }),
            Err(e) => Err(EncodingError {
                line: line.into(),
                description: format!(
                    "encoding check failed for '{}': {}",
                    self.encoding.name(),
                    e
                )
                .into_boxed_str(),
            }),
        }
    }
}

// ── ChainedValidator (private) ────────────────────────────────────────────────

/// Runs two validators in sequence; returns the first `Err` encountered.
struct ChainedValidator {
    first: Arc<dyn EncodingValidator>,
    second: Arc<dyn EncodingValidator>,
}

impl EncodingValidator for ChainedValidator {
    fn validate(&self, line: &str) -> Result<(), EncodingError> {
        self.first.validate(line)?;
        self.second.validate(line)
    }
}

// ── from_grouse ───────────────────────────────────────────────────────────────

/// Convenience constructor: open a grouse [`Source`], decode all lines into a
/// mallard [`LineBuffer`], and install a [`GrouseEncodingValidator`] so that
/// future edits are constrained to the source encoding.
///
/// # Parameters
///
/// - `source` — a fully-probed grouse `Source` (encoding and line-ending
///   already resolved).
/// - `extra` — an optional *additional* validator that is run after the
///   `GrouseEncodingValidator`.  Pass `None` when only encoding enforcement
///   is needed.
///
/// # Errors
///
/// Returns `Err(EncodingError)` when any line from `source` fails validation.
/// Because the lines are decoded from `source`'s own encoding, this only
/// happens when `extra` rejects a line.
pub fn from_grouse(
    source: Source,
    extra: Option<Arc<dyn EncodingValidator>>,
) -> Result<LineBuffer, EncodingError> {
    let encoding = source.encoding();
    let lines = source
        .as_lines()
        .expect("Source::as_lines is currently infallible");
    let line_source = GrouseLineSource::new(lines);

    let primary: Arc<dyn EncodingValidator> = Arc::new(GrouseEncodingValidator::new(encoding));
    let validator: Arc<dyn EncodingValidator> = match extra {
        None => primary,
        Some(v) => Arc::new(ChainedValidator {
            first: primary,
            second: v,
        }),
    };

    LineBuffer::from_source(&line_source, Some(validator))
}
