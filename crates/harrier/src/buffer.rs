// Copyright (c) 2026, Michael Grier

//! `Buffer` — random-access, line-map–backed editor buffer.
//!
//! [`Buffer`] wraps a [`Source`] and a lazy segmented [`LineMap`].  It is the
//! primary type for editor-style workflows that need random access by line
//! number, byte offset, or multi-line byte range.
//!
//! # Construction
//!
//! Use [`Source::as_buffer`] to convert an opened [`Source`] into a `Buffer`.
//! If you want to receive [`LineMapEvent`] notifications, call
//! [`Buffer::with_sender`] on the returned value before performing any scans.
//!
//! # Line-map scanning
//!
//! The internal [`LineMap`] is scanned lazily: operations that need line-number
//! information (e.g. [`Buffer::line_offset`]) drive `scan_next_segment` until
//! sufficient segments are exact.  No background threads are started; all
//! scanning happens synchronously on the calling thread.

use std::{
    ops::Range,
    sync::{Arc, mpsc},
};

use encoding_rs::Encoding;
use redwing::Branch;

use crate::{
    encoded::Encoded, encoding::LineEnding, line_map::LineMap, line_map_event::LineMapEvent,
    lines::DEFAULT_VIEW_CEILING, offset_map::build_offset_map, source::Source, view::View,
};

// ── MA-50: BufferError ────────────────────────────────────────────────────────

/// Errors that can occur while constructing or using a [`Buffer`].
#[derive(Debug)]
pub enum BufferError {
    /// An I/O error occurred while reading bytes from the underlying branch.
    Io(std::io::Error),

    /// The requested byte range would require materialising more bytes than
    /// the configured memory ceiling allows.
    ///
    /// `requested` is the number of bytes in the range; `ceiling` is the
    /// maximum the caller has permitted.
    RangeExceedsCeiling { requested: u64, ceiling: u64 },

    /// The requested line number is past the end of the file.
    ///
    /// `line` is the 0-based line number requested; `total` is the number of
    /// lines in the file (exact at this point).
    LineOutOfRange { line: usize, total: usize },

    /// The [`View`] passed to [`Buffer::apply_edit`] was created from a
    /// previous branch state and is no longer valid against the buffer's
    /// current branch.
    ///
    /// Re-create the view (e.g. via [`Buffer::view_range`] or
    /// [`Buffer::line_content`]) after any structural edit and retry.
    StaleView,

    /// The encoding of the source does not have a usable encoder in
    /// `encoding_rs`, so `Buffer` cannot write bytes back without corrupting
    /// the file.
    ///
    /// This covers UTF-16LE and UTF-16BE: `encoding_rs` delegates their
    /// `new_encoder()` to the UTF-8 encoder, so any bytes produced by
    /// [`Encoded::encode`](crate::encoded::Encoded::encode) or the edit helpers
    /// would be UTF-8 rather than UTF-16, silently corrupting the source.
    ///
    /// For read-only access to UTF-16 files use [`Source::as_chars`] or
    /// [`Source::as_lines`] instead.
    ///
    /// `encoding_name` is the WHATWG name of the offending encoding.
    EncodeUnsupported { encoding_name: &'static str },
}

impl std::fmt::Display for BufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BufferError::Io(e) => write!(f, "I/O error in Buffer: {e}"),
            BufferError::RangeExceedsCeiling { requested, ceiling } => write!(
                f,
                "byte range ({requested} bytes) exceeds memory ceiling ({ceiling} bytes)"
            ),
            BufferError::LineOutOfRange { line, total } => {
                write!(f, "line {line} is out of range (file has {total} lines)")
            }
            BufferError::StaleView => write!(
                f,
                "View was created from a stale branch state; re-create it after the previous edit"
            ),
            BufferError::EncodeUnsupported { encoding_name } => write!(
                f,
                "encoding '{encoding_name}' has no usable encoder in encoding_rs; \
                 use Source::as_chars or Source::as_lines for read-only access"
            ),
        }
    }
}

impl std::error::Error for BufferError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BufferError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for BufferError {
    fn from(e: std::io::Error) -> Self {
        BufferError::Io(e)
    }
}

// ── MA-51: Buffer struct ──────────────────────────────────────────────────────

/// A random-access, line-map–backed editor buffer.
///
/// `Buffer` combines the resolved encoding metadata from a [`Source`] with a
/// lazy segmented [`LineMap`] that provides O(log n) line-to-offset and
/// offset-to-line-col lookups once the relevant segments are scanned.
///
/// # Thread safety
///
/// `Buffer` is not `Send` or `Sync` by default (because `LineMap` holds a
/// mutable `Vec` of segments).  Use external synchronisation if you need to
/// share a buffer across threads.
pub struct Buffer {
    /// The underlying byte stream.
    branch: Arc<dyn Branch>,
    /// The WHATWG encoding for this source.
    encoding: &'static Encoding,
    /// Number of BOM bytes at the start of the branch to skip when reading
    /// content.
    bom_len: usize,
    /// The dominant line-ending convention for this source.
    line_ending: LineEnding,
    /// The lazy segmented line map.  Scanned on demand by Buffer operations.
    line_map: LineMap,
    /// Maximum number of bytes [`view_range`](Buffer::view_range) will
    /// materialise into a single [`View`].
    view_ceiling: u64,
    /// Cached event sender so the line map can be rebuilt after structural edits
    /// while still delivering notifications to the same receiver.
    sender: Option<mpsc::Sender<LineMapEvent>>,
}

impl Buffer {
    /// Construct a `Buffer` from a `Source`, with no event sender.
    ///
    /// This is the internal constructor called by [`Source::as_buffer`].
    /// Use [`Buffer::with_sender`] to attach an event channel afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`BufferError::EncodeUnsupported`] when the source encoding
    /// lacks a usable encoder in `encoding_rs` (UTF-16LE and UTF-16BE).
    /// For read-only access to those sources use [`Source::as_chars`] or
    /// [`Source::as_lines`] instead.
    pub(crate) fn from_source(source: Source) -> Result<Self, BufferError> {
        let encoding = source.encoding();
        if std::ptr::eq(encoding, encoding_rs::UTF_16LE)
            || std::ptr::eq(encoding, encoding_rs::UTF_16BE)
        {
            return Err(BufferError::EncodeUnsupported {
                encoding_name: encoding.name(),
            });
        }
        let branch = source.branch();
        let bom_len = source.bom_len();
        let line_ending = source.line_ending();
        let line_map = LineMap::new(Arc::clone(&branch), None, None);
        Ok(Buffer {
            branch,
            encoding,
            bom_len,
            line_ending,
            line_map,
            view_ceiling: DEFAULT_VIEW_CEILING,
            sender: None,
        })
    }

    /// Attach an event sender and return `self`.
    ///
    /// Replaces the internal [`LineMap`] with one that forwards
    /// [`LineMapEvent`] notifications to `sender`.  Any segments scanned
    /// before this call are preserved; future scans and invalidations will
    /// fire events through the new sender.
    ///
    /// If segments have already been scanned before this call, the caller
    /// will not receive retrospective events for them.
    pub fn with_sender(mut self, sender: mpsc::Sender<LineMapEvent>) -> Self {
        self.sender = Some(sender.clone());
        self.line_map = LineMap::new(Arc::clone(&self.branch), None, Some(sender));
        self
    }

    /// Set the maximum number of bytes that [`view_range`](Buffer::view_range)
    /// will materialise into a single [`View`] and return `self`.
    ///
    /// The default is [`DEFAULT_VIEW_CEILING`] (64 MiB).  Pass a smaller
    /// ceiling to protect against accidentally materialising very large ranges.
    pub fn with_view_ceiling(mut self, ceiling: u64) -> Self {
        self.view_ceiling = ceiling;
        self
    }

    // ── Accessors ─────────────────────────────────────────────────────────────

    /// The branch this `Buffer` is backed by.
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

    /// The number of BOM bytes at the start of the branch (may be 0).
    pub fn bom_len(&self) -> usize {
        self.bom_len
    }

    /// A reference to the internal lazy line map (read-only).
    pub fn line_map(&self) -> &LineMap {
        &self.line_map
    }

    /// A mutable reference to the internal lazy line map.
    ///
    /// Exposed for future structural-edit operations (MA-57) that must call
    /// `invalidate_from_byte` and then mutate the branch.
    pub fn line_map_mut(&mut self) -> &mut LineMap {
        &mut self.line_map
    }

    // ── MA-54: line_offset ────────────────────────────────────────────────────

    /// Return the byte offset of the start of `line` (0-based).
    ///
    /// Drives [`LineMap::scan_next_segment`] as needed until the segment that
    /// contains `line` has been fully scanned.  Returns
    /// [`BufferError::LineOutOfRange`] when `line` is past the last line.
    ///
    /// Line 0 always starts at byte 0; BOM-skipping is the caller's
    /// responsibility (`bom_len()` reports the BOM size).
    pub fn line_offset(&mut self, line: usize) -> Result<u64, BufferError> {
        self.line_map
            .line_offset(line)
            .map_err(BufferError::Io)?
            .ok_or_else(|| {
                // All segments are scanned; find the exact total line count.
                let lc = self.line_map.current_line_count();
                let total = lc.value;
                BufferError::LineOutOfRange { line, total }
            })
    }

    // ── MA-55: offset_to_line_col ─────────────────────────────────────────────

    /// Return the `(line, col)` pair for the given byte `offset` (both 0-based).
    ///
    /// `col` is the raw byte distance from the start of the line to `offset` in
    /// the un-normalised (source) byte stream.  Callers that need a display
    /// column should decode the bytes `[line_start..offset]`.
    ///
    /// Drives [`LineMap::scan_next_segment`] until the segment containing
    /// `offset` is exact.  Returns [`BufferError::Io`] on read failure, or
    /// `Ok((0, 0))` when the branch is empty and `offset == 0`.  Returns
    /// [`BufferError::LineOutOfRange`] when `offset` is beyond the branch.
    pub fn offset_to_line_col(&mut self, offset: u64) -> Result<(usize, usize), BufferError> {
        self.line_map
            .offset_to_line_col(offset)
            .map_err(BufferError::Io)?
            .ok_or_else(|| {
                let total = self.line_map.current_line_count().value;
                BufferError::LineOutOfRange { line: total, total }
            })
    }

    // ── MA-56: line_content ───────────────────────────────────────────────────

    /// Return a [`View`] of line `line` (0-based) in normalised (LF-only) bytes.
    ///
    /// The view covers `[line_start, next_line_start)`, so the terminator byte
    /// (now always `\n` in normalised form) is included for terminated lines.
    /// For the final line of a file that has no trailing newline the view ends
    /// at the branch EOF.
    ///
    /// # Errors
    ///
    /// - [`BufferError::LineOutOfRange`] if `line` exceeds the total line count.
    /// - [`BufferError::Io`] on branch read failure.
    pub fn line_content(&mut self, line: usize) -> Result<View, BufferError> {
        let start = self.line_offset(line)?;

        // Determine the end byte: start of the next line, or branch EOF.
        let end = match self
            .line_map
            .line_offset(line + 1)
            .map_err(BufferError::Io)?
        {
            Some(off) => off,
            None => self.branch.byte_len(),
        };

        let byte_range: Range<u64> = start..end;
        let len = end - start;

        // Read the raw source bytes for this line.
        let mut raw = vec![0u8; len as usize];
        if len > 0 {
            self.branch
                .read_at(byte_range.start, &mut raw)
                .map_err(BufferError::Io)?;
        }

        // Build the offset map (drift table) for this byte range.
        let map =
            build_offset_map(self.branch.as_ref(), byte_range.clone()).map_err(BufferError::Io)?;

        // Normalise CR/CRLF → LF, same as view_range.
        // UTF-16LE/BE sources are rejected at Buffer construction, so any
        // encoding that reaches here and is not ASCII-compatible is treated
        // as opaque (leave bytes as-is) for safety.
        //
        // `is_ascii_compatible()` returns true for UTF-8, all single-byte
        // encodings, and ASCII-superset MBCS encodings (Shift_JIS, Big5,
        // GB18030, EUC-JP, EUC-KR, etc.) — i.e. every encoding where 0x0D
        // and 0x0A can only appear as standalone single bytes and never as
        // part of a larger code unit.
        let ascii_compatible = self.encoding.is_ascii_compatible();
        let normalised = if ascii_compatible {
            let mut out = Vec::with_capacity(raw.len());
            let mut i = 0;
            while i < raw.len() {
                match raw[i] {
                    b'\r' => {
                        out.push(b'\n');
                        if i + 1 < raw.len() && raw[i + 1] == b'\n' {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    b => {
                        out.push(b);
                        i += 1;
                    }
                }
            }
            out
        } else {
            raw
        };

        Ok(View::new(
            normalised,
            map,
            Arc::clone(&self.branch),
            byte_range.start,
        ))
    }

    // ── MA-58: view_range ─────────────────────────────────────────────────────

    /// Return a [`View`] of an arbitrary byte range in normalised (LF-only) bytes.
    ///
    /// The `byte_range` is source-relative (before normalisation).  The
    /// returned [`View`] contains the normalised bytes and an [`OffsetMap`]
    /// for translating normalised positions back to source coordinates for
    /// use with [`View::apply`].
    ///
    /// # Errors
    ///
    /// - [`BufferError::RangeExceedsCeiling`] if the range length exceeds the
    ///   configured `view_ceiling` (default 64 MiB; override with
    ///   [`Buffer::with_view_ceiling`]).
    /// - [`BufferError::Io`] on branch read failure.
    ///
    /// [`OffsetMap`]: crate::offset_map::OffsetMap
    pub fn view_range(&self, byte_range: Range<u64>) -> Result<View, BufferError> {
        // Clamp the requested range to the branch before doing anything else.
        // A caller that navigates off the end of the file (or passes an
        // inverted range) must never be able to drive an out-of-bounds read,
        // fabricate phantom trailing NUL bytes, or trip the ceiling check on a
        // length larger than the file. `read_at` already short-reads past EOF,
        // but clamping keeps the allocation, the ceiling check, and the offset
        // map all sized to bytes that actually exist.
        let branch_len = self.branch.byte_len();
        let start = byte_range.start.min(branch_len);
        let end = byte_range.end.clamp(start, branch_len);
        let byte_range = start..end;
        let len = end - start;

        if len > self.view_ceiling {
            return Err(BufferError::RangeExceedsCeiling {
                requested: len,
                ceiling: self.view_ceiling,
            });
        }

        // Guard the u64 → usize cast used for the allocation below. On 64-bit
        // targets this is a no-op (usize == u64); on 32-bit targets it rejects
        // a range too large to address in memory instead of silently
        // truncating the buffer size.
        let len_usize = usize::try_from(len).map_err(|_| BufferError::RangeExceedsCeiling {
            requested: len,
            ceiling: self.view_ceiling,
        })?;

        // Read the raw source bytes.
        let mut raw = vec![0u8; len_usize];
        if len > 0 {
            self.branch
                .read_at(byte_range.start, &mut raw)
                .map_err(BufferError::Io)?;
        }

        // Build the offset map (drift table) for the range.
        let map =
            build_offset_map(self.branch.as_ref(), byte_range.clone()).map_err(BufferError::Io)?;

        // Normalise CR/CRLF → LF.
        //
        // Byte-level rewriting is only safe for ASCII-compatible encodings,
        // where `\r` (0x0D) and `\n` (0x0A) appear as standalone single bytes
        // and never as part of a multi-byte code unit.  For multi-byte
        // encodings such as UTF-16LE/BE a CRLF code-unit pair is encoded as
        // e.g. `0D 00 0A 00`, and rewriting individual `0x0D`/`0x0A` bytes
        // would produce invalid sequences and corrupt the resulting `View`.
        //
        // `is_ascii_compatible()` returns true for UTF-8, all single-byte
        // encodings, and ASCII-superset MBCS encodings (Shift_JIS, Big5,
        // GB18030, EUC-JP, EUC-KR, etc.) — i.e. every encoding where 0x0D
        // and 0x0A can only appear as standalone single bytes.  UTF-16LE/BE
        // are already blocked at Buffer construction, but `is_ascii_compatible()`
        // would correctly return false for them in any case.
        let ascii_compatible = self.encoding.is_ascii_compatible();
        let normalised = if ascii_compatible {
            let mut out = Vec::with_capacity(raw.len());
            let mut i = 0;
            while i < raw.len() {
                match raw[i] {
                    b'\r' => {
                        out.push(b'\n');
                        if i + 1 < raw.len() && raw[i + 1] == b'\n' {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    b => {
                        out.push(b);
                        i += 1;
                    }
                }
            }
            out
        } else {
            raw
        };

        Ok(View::new(
            normalised,
            map,
            Arc::clone(&self.branch),
            byte_range.start,
        ))
    }

    // ── MA-57: structural line operations ─────────────────────────────────────

    /// The source line-ending bytes for this buffer (LF, CRLF, or CR),
    /// encoded in the buffer's source encoding.
    ///
    /// For ASCII-compatible encodings (UTF-8, ISO-8859-*, Windows-125*, etc.)
    /// this is just the raw `\n` / `\r\n` / `\r` bytes.  For multi-byte
    /// encodings such as UTF-16LE/BE the terminator is encoded into the
    /// source encoding so that inserts produced by `split_line` /
    /// `insert_line` / `append_line` do not corrupt the file by mixing
    /// encodings.
    fn terminator_bytes(&self) -> Vec<u8> {
        let s: &str = match self.line_ending {
            LineEnding::Lf => "\n",
            LineEnding::CrLf => "\r\n",
            LineEnding::Cr => "\r",
        };

        // Fast path: ASCII-compatible single-byte-per-ASCII-char encodings
        // produce identical bytes to the UTF-8 representation for `\n`/`\r`.
        // `encoding_rs` handles this correctly for UTF-16 (LE/BE) and any
        // other multi-byte encoding by emitting the proper code units.
        let mut encoder = self.encoding.new_encoder();
        let max_len = encoder
            .max_buffer_length_from_utf8_without_replacement(s.len())
            .unwrap_or(s.len() * 4);
        let mut out = Vec::with_capacity(max_len);
        let (_result, _read) =
            encoder.encode_from_utf8_to_vec_without_replacement(s, &mut out, true);
        out
    }

    /// Replace the buffer's branch with `new_branch` and rebuild the line map.
    ///
    /// All previously scanned segment data is discarded.  The new line map is
    /// lazy so segments will be re-scanned on demand.
    fn replace_branch(&mut self, new_branch: Arc<dyn Branch>) {
        self.line_map = LineMap::new(Arc::clone(&new_branch), None, self.sender.clone());
        self.branch = new_branch;
    }

    /// Split line `line` at byte column `byte_col` by inserting the file's
    /// native line terminator at that position.
    ///
    /// `byte_col` is the number of raw source bytes from the start of the line
    /// to the desired split point (before normalisation).  After the call the
    /// buffer contains one more line and the line map is rebuilt from scratch.
    ///
    /// # Errors
    ///
    /// - [`BufferError::LineOutOfRange`] if `line` does not exist.
    /// - [`BufferError::Io`] on branch read or edit failure.
    pub fn split_line(&mut self, line: usize, byte_col: usize) -> Result<(), BufferError> {
        let line_start = self.line_offset(line)?;
        let insert_at = line_start + byte_col as u64;
        let term = self.terminator_bytes();
        let fork = self.branch.fork();
        fork.insert_before(insert_at, &term)
            .map_err(BufferError::Io)?;
        self.replace_branch(fork);
        Ok(())
    }

    /// Insert a new empty line (just the file's native terminator) before line
    /// number `at`.
    ///
    /// All lines at index `at` and beyond are shifted down by one.  The line
    /// map is rebuilt from scratch after the insert.
    ///
    /// # Errors
    ///
    /// - [`BufferError::LineOutOfRange`] if `at` is past the last valid line.
    /// - [`BufferError::Io`] on branch read or edit failure.
    pub fn insert_line(&mut self, at: usize) -> Result<(), BufferError> {
        let at_byte = self.line_offset(at)?;
        let term = self.terminator_bytes();
        let fork = self.branch.fork();
        fork.insert_before(at_byte, &term)
            .map_err(BufferError::Io)?;
        self.replace_branch(fork);
        Ok(())
    }

    /// Append a new empty line (just the file's native terminator) at the end
    /// of the buffer.
    ///
    /// # Errors
    ///
    /// - [`BufferError::Io`] on branch edit failure.
    pub fn append_line(&mut self) -> Result<(), BufferError> {
        let eof = self.branch.byte_len();
        let term = self.terminator_bytes();
        let fork = self.branch.fork();
        fork.insert_before(eof, &term).map_err(BufferError::Io)?;
        self.replace_branch(fork);
        Ok(())
    }

    /// Apply an edit described by a `View` and a `normalised_range` to this
    /// buffer, replacing those normalised bytes with `replacement`.
    ///
    /// This is equivalent to calling [`View::apply`] and then updating the
    /// buffer's underlying branch and line map to reflect the change.  Use
    /// this method when you want edits made through `view_range` /
    /// `line_content` views to be reflected in the same `Buffer` instance.
    ///
    /// # Errors
    ///
    /// - [`BufferError::StaleView`] if `view` was created from a previous
    ///   branch state (i.e. an edit has occurred since the view was made).
    /// - [`BufferError::Io`] on branch edit failure.
    pub fn apply_edit(
        &mut self,
        view: &View,
        normalised_range: Range<u64>,
        replacement: &[u8],
    ) -> Result<(), BufferError> {
        // Reject views created from an older branch state.  Without this
        // check, applying a stale view would fork the *old* branch and
        // silently overwrite `self.branch`, discarding intervening edits.
        if !Arc::ptr_eq(&view.branch(), &self.branch) {
            return Err(BufferError::StaleView);
        }
        let new_branch = view
            .apply(normalised_range, replacement)
            .map_err(BufferError::Io)?;
        self.replace_branch(new_branch);
        Ok(())
    }
}

// ── MA-53: Encoded for Buffer ─────────────────────────────────────────────────

impl Encoded for Buffer {
    fn branch(&self) -> Arc<dyn Branch> {
        Arc::clone(&self.branch)
    }

    fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
}
