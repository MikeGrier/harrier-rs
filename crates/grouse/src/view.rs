// Copyright (c) 2026, Michael Grier

//! `View` — normalised-byte window into a branch with position back-translation.
//!
//! A `View` is the result of materialising a byte range from a branch and
//! normalising its line endings to LF-only.  It holds the normalised bytes,
//! an [`OffsetMap`] that translates normalised positions back to source
//! positions, the originating branch, and the branch-absolute start offset of
//! the scanned range.
//!
//! [`View::apply`] accepts a replacement described in normalised coordinates
//! and returns a new [`Arc<dyn Branch>`] with the replacement spliced into
//! the source content at the correct source coordinates.

use std::{ops::Range, sync::Arc};

use redwing::Branch;

use crate::offset_map::OffsetMap;

// ── MA-26: View struct ────────────────────────────────────────────────────────

/// A normalised-byte window into a branch.
///
/// The `bytes` field contains the LF-normalised content for the region
/// `[byte_range_start, byte_range_start + <source_len>)` of the branch.
/// For each normalised byte offset `n`, [`OffsetMap::to_source`] returns the
/// corresponding branch-relative offset within the view's byte range; adding
/// `byte_range_start` gives the branch-absolute source offset.
///
/// # Relationship between normalised and source coordinates
///
/// ```text
/// branch offset = byte_range_start + offset_map.to_source(normalised_offset)
/// ```
///
/// See [`View::apply`] for how this is used during splice operations.
pub struct View {
    /// The normalised (LF-only) bytes of the scanned range.
    pub bytes: Vec<u8>,

    /// Translation table from normalised positions back to branch-relative
    /// source positions within this view's byte range.
    pub offset_map: OffsetMap,

    /// The originating branch whose bytes were normalised.
    branch: Arc<dyn Branch>,

    /// Branch-absolute byte offset at which this view's range begins.
    ///
    /// All `offset_map.to_source()` results must be added to this value to
    /// obtain a branch-absolute source position.
    byte_range_start: u64,
}

impl View {
    /// Construct a `View` from its four components.
    ///
    /// - `bytes` — LF-normalised content for the scanned range.
    /// - `offset_map` — translation table produced by [`build_offset_map`].
    /// - `branch` — the originating branch.
    /// - `byte_range_start` — the branch-absolute start of the scanned range.
    ///
    /// [`build_offset_map`]: crate::offset_map::build_offset_map
    pub fn new(
        bytes: Vec<u8>,
        offset_map: OffsetMap,
        branch: Arc<dyn Branch>,
        byte_range_start: u64,
    ) -> Self {
        View {
            bytes,
            offset_map,
            branch,
            byte_range_start,
        }
    }

    /// Return a clone of the originating branch.
    pub fn branch(&self) -> Arc<dyn Branch> {
        Arc::clone(&self.branch)
    }

    /// Return the branch-absolute byte offset at which this view's range begins.
    pub fn byte_range_start(&self) -> u64 {
        self.byte_range_start
    }

    // ── MA-27: View::apply ────────────────────────────────────────────────────

    /// Splice a replacement into a fork of the originating branch at the source
    /// position that corresponds to `normalised_range`.
    ///
    /// ## Coordinate translation
    ///
    /// `normalised_range` is expressed in the normalised (LF-only) coordinate
    /// space of this `View`.  Each endpoint is translated back to a
    /// branch-absolute source offset:
    ///
    /// ```text
    /// source_start = byte_range_start + offset_map.to_source(normalised_range.start)
    /// source_end   = byte_range_start + offset_map.to_source(normalised_range.end)
    /// source_len   = source_end - source_start
    /// ```
    ///
    /// The `source_len` bytes starting at `source_start` are deleted from a
    /// fresh fork of the originating branch, and `replacement` is inserted at
    /// the same position.
    ///
    /// ## Return value
    ///
    /// Returns an [`Arc<dyn Branch>`] forked from the originating branch with
    /// the replacement applied.  The originating branch is not mutated.
    ///
    /// ## Errors
    ///
    /// - [`std::io::ErrorKind::InvalidInput`] when `normalised_range` is
    ///   inverted (`start > end`) or when `normalised_range.end` exceeds the
    ///   number of bytes in [`View::bytes`].
    /// - Any [`std::io::Error`] from the underlying redwing branch operations
    ///   (`fork`, `splice`).
    pub fn apply(
        &self,
        normalised_range: Range<u64>,
        replacement: &[u8],
    ) -> Result<Arc<dyn Branch>, std::io::Error> {
        let view_len = self.bytes.len() as u64;

        // Reject inverted or out-of-bounds ranges before touching the branch.
        // An inverted range would cause unsigned subtraction to underflow when
        // computing `source_len`; an out-of-bounds end would translate beyond
        // the scanned source span and corrupt the splice position.
        if normalised_range.start > normalised_range.end {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "normalised_range is inverted: start ({}) > end ({})",
                    normalised_range.start, normalised_range.end
                ),
            ));
        }
        if normalised_range.end > view_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "normalised_range.end ({}) exceeds view length ({})",
                    normalised_range.end, view_len
                ),
            ));
        }

        let source_start =
            self.byte_range_start + self.offset_map.to_source(normalised_range.start);
        let source_end = self.byte_range_start + self.offset_map.to_source(normalised_range.end);
        let source_len = source_end - source_start;

        let fork = self.branch.fork();
        fork.splice(source_start, source_len, replacement)?;
        Ok(fork)
    }
}
