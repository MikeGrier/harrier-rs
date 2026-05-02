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

use crate::encoding::LineEnding;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Write the byte sequence that represents `le` to `w`.
///
/// | Variant | Bytes written |
/// |---|---|
/// | [`LineEnding::Lf`]   | `\n` |
/// | [`LineEnding::CrLf`] | `\r\n` |
/// | [`LineEnding::Cr`]   | `\r` |
///
/// This is the canonical place to convert a `LineEnding` discriminant to its
/// raw bytes.  Changing the byte sequences emitted here is a breaking change.
pub(crate) fn write_line_ending(w: &mut impl Write, le: LineEnding) -> io::Result<()> {
    match le {
        LineEnding::Lf => w.write_all(b"\n"),
        LineEnding::CrLf => w.write_all(b"\r\n"),
        LineEnding::Cr => w.write_all(b"\r"),
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
}

impl<W: Write, I: Iterator<Item = LineEnding>> DenormaliseWriter<W, I> {
    /// Create a `DenormaliseWriter` wrapping `inner` and drawing original
    /// terminators from `terminators`.
    ///
    /// `terminators` should yield exactly M items, where M is the number of
    /// line terminators in the original source region that was replaced.
    pub fn new(inner: W, terminators: I) -> Self {
        DenormaliseWriter { inner, terminators }
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
        for le in self.terminators.by_ref() {
            write_line_ending(&mut self.inner, le)?;
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
    /// Write `buf` to the underlying writer, substituting each `\n` byte with
    /// the next terminator from `I`.
    ///
    /// ## M-vs-N terminator preservation rule
    ///
    /// - If `I` still has items when an `\n` is encountered, the `\n` is
    ///   replaced by the next terminator from `I` (which may itself be `\n`
    ///   for LF-sourced lines, `\r\n` for CRLF, or `\r` for CR).
    /// - If `I` is exhausted (M < N case), remaining `\n`s are written as
    ///   plain `\n`.
    ///
    /// Non-`\n` bytes are forwarded verbatim.
    ///
    /// Returns the number of bytes consumed from `buf` (always `buf.len()`
    /// on success; partial writes only occur when the underlying writer
    /// returns an error).
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut i = 0;
        while i < buf.len() {
            // Find the next `\n` in the remaining slice.
            match buf[i..].iter().position(|&b| b == b'\n') {
                None => {
                    // No more newlines — forward the rest verbatim.
                    self.inner.write_all(&buf[i..])?;
                    i = buf.len();
                }
                Some(rel) => {
                    // Write the non-newline prefix (may be empty).
                    if rel > 0 {
                        self.inner.write_all(&buf[i..i + rel])?;
                    }
                    // Substitute or pass through the `\n`.
                    match self.terminators.next() {
                        Some(le) => write_line_ending(&mut self.inner, le)?,
                        None => self.inner.write_all(b"\n")?,
                    }
                    i += rel + 1; // advance past the `\n`
                }
            }
        }
        Ok(buf.len())
    }

    /// Flush the underlying writer.
    ///
    /// Does **not** emit surplus terminators; call [`finish`] for that.
    ///
    /// [`finish`]: DenormaliseWriter::finish
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
