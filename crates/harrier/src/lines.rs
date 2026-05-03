// Copyright (c) 2026, Michael Grier

//! `Lines` — forward-only line iterator over a [`Source`].
//!
//! [`Lines`] wraps a [`Source`], maintains a forward-only byte cursor, and
//! exposes the content of each logical line as a normalised (LF-only) byte
//! slice paired with the original [`LineEnding`] that terminated it.  The
//! final line of a file that has no trailing terminator is yielded with
//! [`LineTerminator::End`].
//!
//! # Construction
//!
//! Use [`Source::as_lines`] to convert an opened `Source` into a `Lines`
//! value.
//!
//! # Forward-only cursor
//!
//! The byte cursor advances monotonically with each call to `next`.  Random
//! access and seeking are not supported; use [`Lines::view_range`] to
//! materialise an arbitrary byte span as a [`View`].

use std::{ops::Range, sync::Arc};

use encoding_rs::Encoding;
use redwing::Branch;

use crate::{
    encoded::Encoded, encoding::LineEnding, offset_map::build_offset_map, source::Source,
    view::View,
};

// ── MA-33: LinesError ────────────────────────────────────────────────────────

/// Errors that can occur while constructing or using a [`Lines`] value.
#[derive(Debug)]
pub enum LinesError {
    /// An I/O error occurred while reading bytes from the underlying branch.
    Io(std::io::Error),

    /// The requested byte range would require materialising more bytes than
    /// the configured memory ceiling allows.
    ///
    /// `requested` is the number of bytes in the range; `ceiling` is the
    /// maximum the caller has permitted.
    RangeExceedsCeiling { requested: u64, ceiling: u64 },
}

impl std::fmt::Display for LinesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinesError::Io(e) => write!(f, "I/O error in Lines: {e}"),
            LinesError::RangeExceedsCeiling { requested, ceiling } => write!(
                f,
                "byte range ({requested} bytes) exceeds memory ceiling ({ceiling} bytes)"
            ),
        }
    }
}

impl std::error::Error for LinesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LinesError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LinesError {
    fn from(e: std::io::Error) -> Self {
        LinesError::Io(e)
    }
}

// ── MA-34: Lines struct ───────────────────────────────────────────────────────

/// The line-ending variant produced by the [`Lines`] iterator for the final
/// (unterminated) line of a source.
///
/// Each `next()` call on [`Lines`] returns a `(Vec<u8>, LineTerminator)` pair.
/// For all but the very last line of a file, the terminator is
/// [`LineTerminator::Ending(le)`].  The last line of a file that has no
/// trailing newline uses [`LineTerminator::End`].
///
/// Changing the variants of this enum is a breaking change for callers that
/// match on the terminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineTerminator {
    /// The line was terminated by `le` in the source.
    Ending(LineEnding),
    /// The line is the final partial line with no trailing terminator.
    End,
}

/// Chunk size for the [`Lines`] iterator read buffer.
const CHUNK: usize = 4096;

/// Default maximum number of bytes that [`Lines::view_range`] will
/// materialise into a single [`View`] unless overridden by
/// [`Lines::with_view_ceiling`].
pub const DEFAULT_VIEW_CEILING: u64 = 64 * 1024 * 1024; // 64 MiB

/// A forward-only line iterator over a [`Source`].
///
/// Yields each logical line as `(normalised_bytes, terminator)`.  CRLFs and
/// lone CRs in the source are delivered as single `\n` bytes in
/// `normalised_bytes`; the original terminator is recorded in the companion
/// `terminator` field.
///
/// # Memory
///
/// One source line is buffered at a time.  The [`Lines`] value itself holds
/// only the scalar cursor and a small fixed-size read buffer.
pub struct Lines {
    /// The originating branch (arc-shared with the `Source`).
    branch: Arc<dyn Branch>,
    /// The WHATWG encoding for this source (carried through for `Encoded`).
    encoding: &'static Encoding,
    /// The dominant line-ending detected for this source.
    line_ending: LineEnding,
    /// Number of BOM bytes to skip at the start of the source.
    /// Preserved from `Source`; cursor starts at this offset after construction.
    #[allow(dead_code)] // stored for completeness; cursor already encodes the skip
    bom_len: usize,
    /// Current byte cursor in the branch.  Advances monotonically; starts
    /// after any BOM bytes.
    cursor: u64,
    /// Maximum bytes [`view_range`](Lines::view_range) will materialise.
    view_ceiling: u64,
    /// Bytes read from the branch that have not yet been formed into a line.
    buf: Vec<u8>,
    /// `true` once the branch has returned 0 bytes (EOF).
    eof: bool,
}

impl Lines {
    /// Construct a `Lines` from a `Source`.
    ///
    /// The cursor is positioned immediately after any BOM bytes detected by
    /// the `Source` probe.
    pub(crate) fn from_source(source: Source) -> Self {
        let bom_len = source.bom_len();
        let encoding = source.encoding();
        let line_ending = source.line_ending();
        let branch = source.branch();
        Lines {
            branch,
            encoding,
            line_ending,
            bom_len,
            cursor: bom_len as u64,
            view_ceiling: DEFAULT_VIEW_CEILING,
            buf: Vec::new(),
            eof: false,
        }
    }

    /// Set the maximum number of bytes that [`view_range`](Lines::view_range)
    /// will materialise into a single [`View`].
    ///
    /// Returns `self` for chaining.
    pub fn with_view_ceiling(mut self, ceiling: u64) -> Self {
        self.view_ceiling = ceiling;
        self
    }

    /// The branch this `Lines` is reading from.
    pub fn branch(&self) -> Arc<dyn Branch> {
        Arc::clone(&self.branch)
    }

    /// The WHATWG encoding of the source.
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// The dominant line-ending convention detected for the source.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// The current byte cursor position (branch-absolute).
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Materialise the source bytes in `byte_range` as a normalised [`View`].
    ///
    /// # Errors
    ///
    /// - [`LinesError::RangeExceedsCeiling`] when the range length exceeds
    ///   [`self.view_ceiling`](DEFAULT_VIEW_CEILING).
    /// - [`LinesError::Io`] on any branch read error.
    pub fn view_range(&self, byte_range: Range<u64>) -> Result<View, LinesError> {
        let len = byte_range.end.saturating_sub(byte_range.start);
        if len > self.view_ceiling {
            return Err(LinesError::RangeExceedsCeiling {
                requested: len,
                ceiling: self.view_ceiling,
            });
        }

        // Read the raw source bytes.
        let mut raw = vec![0u8; len as usize];
        if len > 0 {
            self.branch.read_at(byte_range.start, &mut raw)?;
        }

        // Build the offset map (drift table) for the range.
        let map = build_offset_map(self.branch.as_ref(), byte_range.clone())?;

        // Produce the normalised bytes: CRLF → LF, lone CR → LF.
        let mut normalised = Vec::with_capacity(len as usize);
        let mut i = 0;
        while i < raw.len() {
            match raw[i] {
                b'\r' => {
                    normalised.push(b'\n');
                    if i + 1 < raw.len() && raw[i + 1] == b'\n' {
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                b => {
                    normalised.push(b);
                    i += 1;
                }
            }
        }

        Ok(View::new(
            normalised,
            map,
            Arc::clone(&self.branch),
            byte_range.start,
        ))
    }
}

// ── MA-38: TerminatorLog ─────────────────────────────────────────────────────

/// A fixed-capacity ring-array recording per-line [`LineEnding`] values as
/// the [`Lines`] iterator is consumed.
///
/// When the ring is full, the oldest entry is silently overwritten.  The
/// capacity is set at construction and does not change.  An applications
/// that replaces a contiguous window of lines in a file can size the log to
/// hold exactly as many entries as it needs to cover that window.
///
/// The log is consumed by constructing an [`Iterator`] (via [`iter`]) and
/// passing it to [`DenormaliseWriter`](crate::denormalise::DenormaliseWriter)
/// to re-apply the original per-line terminators to normalised replacement
/// bytes.
///
/// Changing the public API of this type is a breaking change.
///
/// [`iter`]: TerminatorLog::iter
pub struct TerminatorLog {
    /// Storage for entries.  `None` slots are empty (ring is not yet full).
    buf: Vec<Option<LineEnding>>,
    /// Index at which the oldest entry is stored (wraps on push when full).
    head: usize,
    /// Number of entries currently stored (0..=capacity).
    len: usize,
}

impl TerminatorLog {
    /// Create a new `TerminatorLog` with the given fixed `capacity`.
    ///
    /// A capacity of 0 is valid; `push` becomes a no-op and `iter` always
    /// yields nothing.
    pub fn new(capacity: usize) -> Self {
        TerminatorLog {
            buf: vec![None; capacity],
            head: 0,
            len: 0,
        }
    }

    /// Record `le` as the next line terminator.
    ///
    /// If the ring is full the oldest entry is overwritten and the effective
    /// window advances by one.  If the capacity is 0, this call is a no-op.
    pub fn push(&mut self, le: LineEnding) {
        if self.buf.is_empty() {
            return;
        }
        let tail = (self.head + self.len) % self.buf.len();
        self.buf[tail] = Some(le);
        if self.len < self.buf.len() {
            self.len += 1;
        } else {
            // Overwrite oldest: advance head.
            self.head = (self.head + 1) % self.buf.len();
        }
    }

    /// The number of entries currently stored.
    pub fn len(&self) -> usize {
        self.len
    }

    /// `true` when no entries have been pushed or capacity is 0.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The maximum number of entries the ring can hold.
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Iterate over stored entries from oldest to newest.
    ///
    /// The returned iterator clones the entries; the log itself is unchanged.
    pub fn iter(&self) -> TerminatorLogIter<'_> {
        TerminatorLogIter { log: self, pos: 0 }
    }
}

/// Iterator over the entries of a [`TerminatorLog`] from oldest to newest.
pub struct TerminatorLogIter<'a> {
    log: &'a TerminatorLog,
    /// Number of entries already yielded.
    pos: usize,
}

impl<'a> Iterator for TerminatorLogIter<'a> {
    type Item = LineEnding;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.log.len {
            return None;
        }
        let idx = (self.log.head + self.pos) % self.log.buf.len();
        self.pos += 1;
        self.log.buf[idx]
    }
}

// ── MA-36: Encoded for Lines ─────────────────────────────────────────────────

impl Encoded for Lines {
    fn branch(&self) -> Arc<dyn Branch> {
        self.branch()
    }

    fn encoding(&self) -> &'static Encoding {
        self.encoding()
    }

    fn line_ending(&self) -> LineEnding {
        self.line_ending()
    }
}

// ── MA-37: Lines as Iterator ─────────────────────────────────────────────────

impl Lines {
    /// Refill `self.buf` with the next chunk from the branch.
    ///
    /// Sets `self.eof` when the branch is exhausted or returns an I/O error.
    /// Returns `true` if new bytes were added, `false` if already at EOF.
    fn refill(&mut self) -> bool {
        if self.eof {
            return false;
        }
        let mut chunk = [0u8; CHUNK];
        match self.branch.read_at(self.cursor, &mut chunk) {
            Ok(0) => {
                self.eof = true;
                false
            }
            Ok(n) => {
                self.cursor += n as u64;
                self.buf.extend_from_slice(&chunk[..n]);
                true
            }
            Err(_) => {
                // I/O error: treat the remainder of buf as a partial line.
                self.eof = true;
                false
            }
        }
    }

    /// Line iterator body for UTF-16LE (`big_endian = false`) and
    /// UTF-16BE (`big_endian = true`).
    ///
    /// Scans `self.buf` in aligned 2-byte code-unit steps so that the raw
    /// `0x0A` byte that encodes the low half of a LF code unit in UTF-16LE
    /// is never mistaken for an ASCII newline.
    ///
    /// # Output contract
    ///
    /// The yielded `Vec<u8>` contains the raw UTF-16(LE/BE) bytes of the line
    /// *including* a trailing normalised LF code unit (`[0x0A, 0x00]` for LE,
    /// `[0x00, 0x0A]` for BE) for all `LineTerminator::Ending` variants,
    /// matching the single-byte-encoding invariant that callers can decode the
    /// whole slice and receive a `&str` ending in `'\n'`.  The
    /// `LineTerminator::End` variant (no trailing newline in source) does not
    /// append any extra bytes.
    fn next_utf16(&mut self, big_endian: bool) -> Option<(Vec<u8>, LineTerminator)> {
        // Encode a normalised LF in the target byte order.
        let lf_pair: [u8; 2] = if big_endian {
            [0x00, 0x0A]
        } else {
            [0x0A, 0x00]
        };

        loop {
            // ── Scan buf in 2-byte aligned code-unit steps ────────────────
            let mut i = 0;
            while i + 1 < self.buf.len() {
                let (b0, b1) = (self.buf[i], self.buf[i + 1]);

                // Detect LF and CR code units according to byte order.
                let is_lf = if big_endian {
                    b0 == 0x00 && b1 == 0x0A
                } else {
                    b0 == 0x0A && b1 == 0x00
                };
                let is_cr = if big_endian {
                    b0 == 0x00 && b1 == 0x0D
                } else {
                    b0 == 0x0D && b1 == 0x00
                };

                if is_lf {
                    // Bare LF terminator.
                    let mut line = self.buf[..i].to_vec();
                    line.extend_from_slice(&lf_pair);
                    self.buf.drain(..i + 2);
                    return Some((line, LineTerminator::Ending(LineEnding::Lf)));
                } else if is_cr {
                    // CR: need to peek at the next code unit to distinguish
                    // lone CR from CRLF.
                    if i + 3 < self.buf.len() {
                        // Have enough lookahead to decide right now.
                        let (n0, n1) = (self.buf[i + 2], self.buf[i + 3]);
                        let next_is_lf = if big_endian {
                            n0 == 0x00 && n1 == 0x0A
                        } else {
                            n0 == 0x0A && n1 == 0x00
                        };
                        let (consume, le) = if next_is_lf {
                            (i + 4, LineEnding::CrLf)
                        } else {
                            (i + 2, LineEnding::Cr)
                        };
                        let mut line = self.buf[..i].to_vec();
                        line.extend_from_slice(&lf_pair);
                        self.buf.drain(..consume);
                        return Some((line, LineTerminator::Ending(le)));
                    } else if self.eof {
                        // CR is the last code unit in the file: lone CR.
                        let mut line = self.buf[..i].to_vec();
                        line.extend_from_slice(&lf_pair);
                        self.buf.drain(..i + 2);
                        return Some((line, LineTerminator::Ending(LineEnding::Cr)));
                    } else {
                        // Not enough lookahead; fetch more data and rescan.
                        break;
                    }
                } else {
                    i += 2;
                }
            }

            // ── No complete line found in current buf ─────────────────────
            if self.eof {
                if self.buf.is_empty() {
                    return None;
                }
                // Final partial content (no trailing newline in source).
                // May be a single orphan byte if the source is malformed
                // (odd total byte count), which we yield as-is.
                let line = std::mem::take(&mut self.buf);
                return Some((line, LineTerminator::End));
            }

            // ── Refill and rescan ─────────────────────────────────────────
            self.refill();
        }
    }
}

impl Iterator for Lines {
    /// Each item is the normalised bytes of one line paired with the kind of
    /// terminator that ended it.
    ///
    /// - For terminated lines, `normalised_bytes` ends with a normalised LF
    ///   sequence in the source encoding: `b'\n'` for single-byte encodings,
    ///   `[0x0A, 0x00]` for UTF-16LE, `[0x00, 0x0A]` for UTF-16BE.
    ///   The companion [`LineTerminator::Ending`] records the original kind.
    /// - For the final partial line (no trailing newline in source),
    ///   `normalised_bytes` has no trailing newline and the terminator is
    ///   [`LineTerminator::End`].
    /// - Returns `None` when the branch is fully consumed.
    type Item = (Vec<u8>, LineTerminator);

    fn next(&mut self) -> Option<Self::Item> {
        // Dispatch to the encoding-aware UTF-16 scanner when necessary.
        // UTF-16LE encodes LF as [0x0A, 0x00]; the single-byte scanner would
        // split on the bare 0x0A, misaligning every subsequent code unit.
        if self.encoding == encoding_rs::UTF_16LE {
            return self.next_utf16(false);
        }
        if self.encoding == encoding_rs::UTF_16BE {
            return self.next_utf16(true);
        }

        loop {
            // ── Scan buf for the earliest line terminator ─────────────────
            let mut i = 0;
            while i < self.buf.len() {
                match self.buf[i] {
                    b'\r' => {
                        if i + 1 < self.buf.len() {
                            // Enough lookahead: decide CRLF vs lone CR now.
                            let (consume, le) = if self.buf[i + 1] == b'\n' {
                                (i + 2, LineEnding::CrLf)
                            } else {
                                (i + 1, LineEnding::Cr)
                            };
                            let mut line = self.buf[..i].to_vec();
                            line.push(b'\n');
                            self.buf.drain(..consume);
                            return Some((line, LineTerminator::Ending(le)));
                        } else if self.eof {
                            // CR is last byte and no more data: lone CR at end.
                            let mut line = self.buf[..i].to_vec();
                            line.push(b'\n');
                            self.buf.drain(..i + 1);
                            return Some((line, LineTerminator::Ending(LineEnding::Cr)));
                        } else {
                            // CR is last byte; need more data to decide CRLF vs lone CR.
                            break;
                        }
                    }
                    b'\n' => {
                        let mut line = self.buf[..i].to_vec();
                        line.push(b'\n');
                        self.buf.drain(..i + 1);
                        return Some((line, LineTerminator::Ending(LineEnding::Lf)));
                    }
                    _ => {
                        i += 1;
                    }
                }
            }

            // ── No complete line found yet ─────────────────────────────────
            if self.eof {
                if self.buf.is_empty() {
                    return None;
                }
                // Final partial line with no trailing newline.
                let line = std::mem::take(&mut self.buf);
                return Some((line, LineTerminator::End));
            }

            // ── Refill from the branch ─────────────────────────────────────
            self.refill();
        }
    }
}
