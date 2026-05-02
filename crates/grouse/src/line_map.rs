// Copyright (c) 2026, Michael Grier

//! Lazy segmented line map for a source branch.
//!
//! [`LineMap`] partitions a branch into fixed-size segments and scans them on
//! demand, emitting [`LineMapEvent`] notifications through an optional
//! [`mpsc::Sender`].  Unscanned segments contribute extrapolated estimates to
//! the overall [`LineCount`]; once a segment is scanned its contribution
//! becomes exact.

use std::sync::{mpsc, Arc};

use redwing::branch::Branch;

use crate::{
    encoding::LineEnding, line_count::LineCount, line_map_event::LineMapEvent,
    line_map_segment::LineMapSegment,
};

/// Default segment size: 64 KiB.
const DEFAULT_SEGMENT_SIZE: u64 = 64 * 1024;

/// Read buffer size used inside [`LineMap::scan_next_segment`].
const SCAN_BUF: usize = 4096;

/// Lazy, segmented line map over a [`Branch`].
///
/// The map partitions the branch into fixed-size segments ([`LineMapSegment`]).
/// Segments are scanned on demand via [`scan_next_segment`].  Before a segment
/// is scanned its contribution to the overall line count is estimated by
/// extrapolating from already-scanned segments.
///
/// # Events
///
/// If a [`mpsc::Sender<LineMapEvent>`] is supplied at construction time, the
/// map fires:
/// - [`LineMapEvent::LineCountChanged`] after every scan or invalidation that
///   changes the estimate or exact count.
/// - [`LineMapEvent::RegionExact`] after a segment is fully scanned.
/// - [`LineMapEvent::MapInvalidated`] when segments are reset due to an edit.
///
/// Events are fired in the order listed above, once per call.  The sender is
/// silently dropped on send failures (receiver closed).
pub struct LineMap {
    branch: Arc<dyn Branch>,
    segment_size: u64,
    segments: Vec<LineMapSegment>,
    sender: Option<mpsc::Sender<LineMapEvent>>,
}

impl LineMap {
    /// Construct a new, fully-unscanned line map for `branch`.
    ///
    /// `segment_size` controls the byte granularity of each segment.  Pass
    /// `None` to use the default (64 KiB).  `sender` is an optional channel
    /// for receiving [`LineMapEvent`] notifications.
    pub fn new(
        branch: Arc<dyn Branch>,
        segment_size: Option<u64>,
        sender: Option<mpsc::Sender<LineMapEvent>>,
    ) -> Self {
        let seg_size = segment_size.unwrap_or(DEFAULT_SEGMENT_SIZE).max(1);
        let total = branch.byte_len();
        let count = if total == 0 {
            0usize
        } else {
            total.div_ceil(seg_size) as usize
        };
        let mut segments = Vec::with_capacity(count);
        for i in 0..count {
            let start = i as u64 * seg_size;
            let end = (start + seg_size).min(total);
            segments.push(LineMapSegment::unscanned(start..end));
        }
        Self {
            branch,
            segment_size: seg_size,
            segments,
            sender,
        }
    }

    /// Number of segments in the map.
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Borrow segment `idx`.  Panics if `idx` is out of bounds.
    pub fn segment(&self, idx: usize) -> &LineMapSegment {
        &self.segments[idx]
    }

    /// Index of the first unscanned segment, or `None` if all are scanned.
    pub fn next_unscanned(&self) -> Option<usize> {
        self.segments.iter().position(|s| !s.exact)
    }

    /// Scan the next unscanned segment.
    ///
    /// Returns `true` if a segment was scanned, `false` if all segments are
    /// already exact.  After scanning, fires [`LineMapEvent::LineCountChanged`]
    /// and [`LineMapEvent::RegionExact`] as appropriate.
    pub fn scan_next_segment(&mut self) -> bool {
        let Some(idx) = self.next_unscanned() else {
            return false;
        };
        self.scan_segment(idx);
        true
    }

    /// Scan segment at `idx`.  Assumes `idx` is in bounds and not yet exact.
    fn scan_segment(&mut self, idx: usize) {
        let seg = &self.segments[idx];
        let byte_range = seg.byte_range.clone();
        let skip = seg.skip_first_byte;
        let branch_len = self.branch.byte_len();

        let scan_start = byte_range.start + u64::from(skip);
        let scan_end = byte_range.end;

        let mut terminators: Vec<LineEnding> = Vec::new();
        let mut pos = scan_start;
        // `pending_cr` is true when the previous byte was \r and we have not
        // yet seen the byte that resolves whether it is a lone CR or part of a
        // CRLF pair.
        let mut pending_cr = false;
        // `partial` tracks whether the bytes after the last resolved terminator
        // contain any non-terminator content.
        let mut partial = false;
        let mut buf = [0u8; SCAN_BUF];

        while pos < scan_end {
            let to_read = ((scan_end - pos) as usize).min(SCAN_BUF);
            let n = self.branch.read_at(pos, &mut buf[..to_read]).unwrap_or(0);
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                match b {
                    b'\n' => {
                        if pending_cr {
                            terminators.push(LineEnding::CrLf);
                            pending_cr = false;
                        } else {
                            terminators.push(LineEnding::Lf);
                        }
                        partial = false;
                    }
                    b'\r' => {
                        if pending_cr {
                            // Previous \r was a lone CR — resolve it.
                            terminators.push(LineEnding::Cr);
                        }
                        // Mark a new pending \r; defer resolution to next byte.
                        pending_cr = true;
                    }
                    _ => {
                        if pending_cr {
                            // Non-newline after \r → lone CR.
                            terminators.push(LineEnding::Cr);
                            pending_cr = false;
                        }
                        partial = true;
                    }
                }
            }
            pos += n as u64;
        }

        // Resolve any pending \r at the segment boundary.
        let mut next_skip = false;
        if pending_cr {
            if byte_range.end < branch_len {
                // Peek at the first byte of the next segment.
                let mut peek = [0u8; 1];
                if self.branch.read_at(byte_range.end, &mut peek).ok() == Some(1)
                    && peek[0] == b'\n'
                {
                    // Cross-boundary CRLF: attribute to this segment.
                    terminators.push(LineEnding::CrLf);
                    next_skip = true;
                } else {
                    // Lone CR at segment boundary.
                    terminators.push(LineEnding::Cr);
                }
            } else {
                // EOF: lone CR at end of file.
                terminators.push(LineEnding::Cr);
            }
            partial = false;
        }

        // Write results back.
        self.segments[idx].terminators = terminators;
        self.segments[idx].trailing_partial = partial;
        self.segments[idx].exact = true;

        // Propagate skip to the following segment if needed.
        if next_skip && idx + 1 < self.segments.len() {
            self.segments[idx + 1].skip_first_byte = true;
        }

        // Fire events.
        let first_line = self.line_start_of_segment(idx);
        let line_count_in_seg = self.segments[idx].line_count();
        let end_line = first_line + line_count_in_seg.saturating_sub(1);

        if let Some(ref tx) = self.sender {
            let count = self.current_line_count();
            let _ = tx.send(LineMapEvent::LineCountChanged { count });
            let _ = tx.send(LineMapEvent::RegionExact {
                start_line: first_line,
                end_line,
            });
        }
    }

    /// Compute the current best [`LineCount`] estimate.
    ///
    /// If all segments are scanned the count is exact.  Otherwise, the line
    /// density of scanned segments is extrapolated over the remaining bytes.
    pub fn current_line_count(&self) -> LineCount {
        self.line_count_estimate()
    }

    /// Invalidate all segments starting at or after `from_byte`.
    ///
    /// Resets the affected segments to unscanned state and fires
    /// [`LineMapEvent::MapInvalidated`].  `from_byte` is rounded down to the
    /// nearest segment boundary.
    ///
    /// Returns the 0-based line number of the first invalidated line (which is
    /// the value carried by the `MapInvalidated` event).
    pub fn invalidate_from_byte(&mut self, from_byte: u64) -> usize {
        if self.segments.is_empty() {
            return 0;
        }
        let first_seg = (from_byte / self.segment_size) as usize;
        let first_seg = first_seg.min(self.segments.len() - 1);

        let from_line = self.line_start_of_segment(first_seg);

        for (i, seg) in self.segments[first_seg..].iter_mut().enumerate() {
            seg.terminators.clear();
            seg.trailing_partial = false;
            if i != 0 {
                seg.skip_first_byte = false;
            }
            seg.exact = false;
        }

        if let Some(ref tx) = self.sender {
            let _ = tx.send(LineMapEvent::MapInvalidated { from_line });
            let count = self.current_line_count();
            let _ = tx.send(LineMapEvent::LineCountChanged { count });
        }

        from_line
    }

    // ── MA-54: line_offset ────────────────────────────────────────────────────

    /// Return the byte offset of the start of `line` (0-based).
    ///
    /// Scans segments on demand until the segment that contains `line` is
    /// exact.  Returns `None` when `line` is beyond the last line in the file
    /// (i.e. the caller should convert this to `BufferError::LineOutOfRange`).
    ///
    /// Line 0 always starts at byte 0 (BOM skipping is the caller's
    /// responsibility).
    pub fn line_offset(&mut self, line: usize) -> Result<Option<u64>, std::io::Error> {
        if line == 0 {
            // Line 0 always starts at the beginning of the branch.
            return Ok(Some(0));
        }

        // Scan until we have enough exact segments.
        loop {
            // Find which segment owns `line`.
            let mut cum = 0usize;
            let mut found_seg: Option<usize> = None;
            for (i, seg) in self.segments.iter().enumerate() {
                if !seg.exact {
                    break; // not yet scanned
                }
                let seg_lines = seg.line_count();
                // `cum` = first line number of this segment.
                // A line is in this segment if its start byte falls within it,
                // which is true whenever line is within [cum, cum + seg_lines].
                if line <= cum + seg_lines {
                    found_seg = Some(i);
                    break;
                }
                cum += seg_lines;
                // Also account for the trailing partial of the last segment.
                if i + 1 == self.segments.len() && seg.trailing_partial && line == cum {
                    found_seg = Some(i);
                }
            }

            if let Some(seg_idx) = found_seg {
                // Walk branch bytes in segment to find line start.
                return self.byte_offset_of_line_in_segment(seg_idx, line, cum);
            }

            // Scanned segments don't cover `line` yet.  Scan one more.
            if !self.scan_next_segment() {
                // All segments are now scanned; `line` is past EOF.
                return Ok(None);
            }
        }
    }

    /// Walk the bytes of segment `seg_idx` to find the byte offset of `line`.
    ///
    /// `seg_first_line` is the 0-based line number of the first line in this
    /// segment (i.e. the cumulative terminated-line count of all preceding
    /// segments).
    fn byte_offset_of_line_in_segment(
        &self,
        seg_idx: usize,
        line: usize,
        seg_first_line: usize,
    ) -> Result<Option<u64>, std::io::Error> {
        let seg = &self.segments[seg_idx];

        // How many lines into this segment is `line`?
        let lines_into_seg = line - seg_first_line;

        if lines_into_seg == 0 {
            // The line starts right at the segment start (after any skip).
            return Ok(Some(seg.byte_range.start + u64::from(seg.skip_first_byte)));
        }

        // We need to find the byte after the `lines_into_seg`-th terminator in
        // this segment, walking the raw bytes.
        let scan_start = seg.byte_range.start + u64::from(seg.skip_first_byte);
        let scan_end = seg.byte_range.end;

        let mut terminators_seen: usize = 0;
        let mut pos = scan_start;
        let mut pending_cr = false;
        let mut buf = [0u8; SCAN_BUF];

        while pos < scan_end {
            let to_read = ((scan_end - pos) as usize).min(SCAN_BUF);
            let n = self
                .branch
                .read_at(pos, &mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            for (i, &b) in buf[..n].iter().enumerate() {
                let byte_pos = pos + i as u64;
                match b {
                    b'\n' => {
                        terminators_seen += 1;
                        if terminators_seen == lines_into_seg {
                            return Ok(Some(byte_pos + 1));
                        }
                        pending_cr = false;
                    }
                    b'\r' => {
                        if pending_cr {
                            // Resolve previous lone CR.
                            terminators_seen += 1;
                            if terminators_seen == lines_into_seg {
                                return Ok(Some(byte_pos));
                            }
                        }
                        pending_cr = true;
                    }
                    _ => {
                        if pending_cr {
                            terminators_seen += 1;
                            if terminators_seen == lines_into_seg {
                                return Ok(Some(byte_pos));
                            }
                            pending_cr = false;
                        }
                    }
                }
            }
            pos += n as u64;
        }

        // If pending_cr at end (last segment, lone CR at EOF).
        if pending_cr {
            terminators_seen += 1;
            if terminators_seen == lines_into_seg {
                return Ok(Some(scan_end));
            }
        }

        // `line` not found in this segment.
        Ok(None)
    }

    // ── MA-55: offset_to_line_col ─────────────────────────────────────────────

    /// Return the `(line, col)` pair for a given byte `offset` (both 0-based).
    ///
    /// `col` is the byte distance from the start of the line to `offset` in
    /// the normalised (LF-only) view.  Callers that need a display column must
    /// decode the bytes between the line start and `offset`.
    ///
    /// Scans segments on demand until the segment containing `offset` is exact.
    /// Returns `None` if `offset` is beyond the end of the branch.
    pub fn offset_to_line_col(
        &mut self,
        offset: u64,
    ) -> Result<Option<(usize, usize)>, std::io::Error> {
        let branch_len = self.branch.byte_len();
        if offset >= branch_len && branch_len > 0 {
            return Ok(None);
        }
        if branch_len == 0 {
            if offset == 0 {
                return Ok(Some((0, 0)));
            }
            return Ok(None);
        }

        // Find which segment contains `offset`, scanning as needed.
        loop {
            // Binary search for the segment whose byte_range contains `offset`.
            let seg_idx = self
                .segments
                .partition_point(|s| s.byte_range.end <= offset);
            let seg_idx = seg_idx.min(self.segments.len().saturating_sub(1));

            if self.segments[seg_idx].exact {
                return self.line_col_in_segment(seg_idx, offset);
            }

            // Segment not yet exact; scan until it is.
            if !self.scan_next_segment() {
                // All segments scanned but still not found (offset >= branch_len).
                return Ok(None);
            }
        }
    }

    /// Walk segment bytes to determine (line, col) for `offset`.
    fn line_col_in_segment(
        &self,
        seg_idx: usize,
        offset: u64,
    ) -> Result<Option<(usize, usize)>, std::io::Error> {
        let seg_first_line = self.line_start_of_segment(seg_idx);
        let seg = &self.segments[seg_idx];

        let scan_start = seg.byte_range.start + u64::from(seg.skip_first_byte);

        let mut line = seg_first_line;
        let mut line_start = scan_start;
        let mut pos = scan_start;
        let mut pending_cr = false;
        let mut buf = [0u8; SCAN_BUF];

        // Walk bytes up to (but not including) `offset`, counting line breaks.
        let scan_end = offset.min(seg.byte_range.end);

        while pos < scan_end {
            let to_read = ((scan_end - pos) as usize).min(SCAN_BUF);
            let n = self
                .branch
                .read_at(pos, &mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            for (i, &b) in buf[..n].iter().enumerate() {
                let byte_pos = pos + i as u64;
                if byte_pos >= offset {
                    break;
                }
                match b {
                    b'\n' => {
                        line += 1;
                        line_start = byte_pos + 1;
                        pending_cr = false;
                    }
                    b'\r' => {
                        if pending_cr {
                            // Previous lone CR was a line terminator.
                            line += 1;
                            line_start = byte_pos;
                        }
                        pending_cr = true;
                    }
                    _ => {
                        if pending_cr {
                            line += 1;
                            line_start = byte_pos;
                            pending_cr = false;
                        }
                    }
                }
            }
            pos += n as u64;
        }

        // Compute normalised column: for CRLF/CR lines the \r is the line
        // terminator byte but the *next* line starts at \r+1 (for CR) or
        // \r+2 (for CRLF).  The column is simply the raw byte distance from
        // `line_start` to `offset`.
        let col = (offset - line_start) as usize;
        Ok(Some((line, col)))
    }

    // ── helpers ───────────────────────────────────────────────────────────────

    /// 0-based line number of the first line in segment `idx`.
    fn line_start_of_segment(&self, idx: usize) -> usize {
        self.segments[..idx].iter().map(|s| s.line_count()).sum()
    }

    /// MA-47: density-extrapolation estimate for the total line count.
    fn line_count_estimate(&self) -> LineCount {
        let total_bytes = self.branch.byte_len();

        if total_bytes == 0 {
            return LineCount::exact(0);
        }

        let mut scanned_bytes: u64 = 0;
        let mut scanned_lines: usize = 0;
        let mut all_exact = true;

        for seg in &self.segments {
            if seg.exact {
                scanned_bytes += seg.byte_len();
                scanned_lines += seg.line_count();
            } else {
                all_exact = false;
            }
        }

        if all_exact {
            // Total terminated lines + 1 if the last segment has an
            // unterminated trailing line (file does not end with a newline).
            let partial = self.segments.last().is_some_and(|s| s.trailing_partial);
            let total = scanned_lines + usize::from(partial);
            return LineCount::exact(total);
        }

        if scanned_bytes == 0 {
            // No data yet — fall back to byte-density of 1 line per 40 bytes.
            let estimate = total_bytes.div_ceil(40) as usize;
            return LineCount::estimate(estimate.max(1));
        }

        // Extrapolate: lines_per_byte * total_bytes, rounded up.
        // Use integer arithmetic: estimate = scanned_lines * total_bytes / scanned_bytes.
        let estimate =
            ((scanned_lines as u128 * total_bytes as u128) / scanned_bytes as u128) as usize;
        LineCount::estimate(estimate.max(1))
    }
}
