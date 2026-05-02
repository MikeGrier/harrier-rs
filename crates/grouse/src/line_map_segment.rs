// Copyright (c) 2026, Michael Grier

use std::ops::Range;

use crate::encoding::LineEnding;

/// A contiguous byte span of a source branch whose line-terminator data may be
/// fully known (`exact: true`) or not yet scanned (`exact: false`).
///
/// Segments partition the source branch into fixed-size chunks.  Each segment
/// independently tracks the per-line [`LineEnding`] kinds found within its byte
/// range.  Together they form the backing storage for the lazy segmented line
/// map.
///
/// # Boundary handling
///
/// A CRLF sequence that straddles a segment boundary (`\r` is the last byte of
/// one segment, `\n` is the first byte of the next) is attributed to the
/// *first* segment: when the scanner peeks one byte past the end of its range
/// and finds `\n`, it records `CrLf` and sets `skip_first_byte = true` on the
/// next segment so its own scan starts at `byte_range.start + 1`.
///
/// Changing any field meaning is a breaking change for callers that serialise
/// or cache segment data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineMapSegment {
    /// The byte span (half-open, source-relative) that this segment covers.
    pub byte_range: Range<u64>,

    /// Terminator kind for each line whose terminator falls entirely within
    /// `byte_range`.  One entry per terminated line; lines are stored in
    /// source order.
    pub terminators: Vec<LineEnding>,

    /// `true` if the last byte of `byte_range` falls in the middle of a line
    /// (no terminator was found for the final partial line in this segment).
    /// For unscanned segments this is always `false`.  For the last segment of
    /// a file that does not end with a newline this will be `true`.
    pub trailing_partial: bool,

    /// `true` when this segment's scan should begin at `byte_range.start + 1`
    /// because the preceding segment already consumed that byte as the `\n`
    /// half of a cross-boundary `\r\n`.
    pub skip_first_byte: bool,

    /// `true` once this segment has been fully scanned.
    pub exact: bool,
}

impl LineMapSegment {
    /// Create an unscanned segment covering `byte_range`.
    pub fn unscanned(byte_range: Range<u64>) -> Self {
        Self {
            byte_range,
            terminators: Vec::new(),
            trailing_partial: false,
            skip_first_byte: false,
            exact: false,
        }
    }

    /// Number of lines whose terminator falls within this segment.
    ///
    /// Lines that straddle a segment boundary are counted in the segment where
    /// their terminator appears — not in the segment where they start.  A
    /// line that has no terminator (the partial tail at the end of a segment)
    /// is NOT counted here; the map adds such a line separately only when the
    /// last scanned segment has `trailing_partial == true`.
    ///
    /// For an unscanned segment the result is always 0.
    pub fn line_count(&self) -> usize {
        self.terminators.len()
    }

    /// Number of bytes covered by this segment.
    pub fn byte_len(&self) -> u64 {
        self.byte_range.end - self.byte_range.start
    }
}
