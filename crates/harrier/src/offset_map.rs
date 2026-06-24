// Copyright (c) 2026, Michael Grier

//! `OffsetMap` — normalised-to-source position translation.
//!
//! When a byte stream containing CRLF or lone-CR line endings is normalised to
//! LF-only, each two-byte CRLF sequence becomes a single LF byte.  This
//! shrinks the stream: for every CRLF, every normalised position *after* the
//! replacement is one less than the corresponding source position.
//!
//! `OffsetMap` records the cumulative drift introduced by these replacements as
//! a sorted list of `(normalised_pos, cumulative_drift)` pairs.  The pair
//! `(p, d)` means: for every normalised position `n >= p` (up to the next
//! recorded pair, or end-of-stream), the corresponding source position is
//! `n + d`.
//!
//! # Construction
//!
//! - [`OffsetMap::identity()`] — zero-drift map for all-LF sources.
//! - [`build_offset_map`] — scans a branch byte range and records one drift
//!   entry per CRLF found.
//!
//! # Translation
//!
//! [`OffsetMap::to_source`] converts a normalised byte offset back to the
//! corresponding source byte offset via binary search over the drift table.
//!
//! # Drift entry placement
//!
//! An entry is recorded at the normalised position **immediately after** the
//! replacement LF byte.  For example, a CRLF at source positions `[s, s+1]`
//! becomes a single LF at normalised position `n`.  The drift entry is placed
//! at `(n + 1, d)` so that:
//! - `to_source(n)` returns `n + 0 = s` (the CR at the start of the CRLF), and
//! - `to_source(n + 1)` returns `(n + 1) + 1 = s + 2` (the first byte after
//!   the CRLF in source).
//!
//! This convention ensures that the normalised LF maps to the first byte of the
//! original line-ending sequence in source, which is the least-surprising
//! behaviour for cursor/span translation.
//!
//! Changing the placement convention is a breaking change.

use std::ops::Range;

use redwing::Branch;

// ── MA-24: OffsetMap ─────────────────────────────────────────────────────────

/// Translation table from normalised byte positions (LF-only) to source byte
/// positions.
///
/// Each entry `(normalised_pos, cumulative_drift)` marks the first normalised
/// position at which all subsequent positions share the given cumulative drift.
/// The drift increases by one for every CRLF sequence replaced by a single LF.
///
/// The `entries` slice is always sorted in strictly ascending order by
/// `normalised_pos`.  Changing the sort order or the drift arithmetic is a
/// breaking change.
pub struct OffsetMap {
    /// Sorted `(normalised_pos, cumulative_drift)` pairs.
    ///
    /// An entry `(p, d)` means: for normalised offsets `n` where `p <= n <
    /// next_entry.normalised_pos`, `source = n + d`.
    ///
    /// Changing this field or its ordering invariant is a breaking change.
    entries: Vec<(u64, i64)>,
}

impl OffsetMap {
    /// Return an `OffsetMap` with no drift entries — suitable for sources that
    /// use LF-only line endings and require no position translation.
    ///
    /// For an identity map, [`to_source`](Self::to_source)`(n)` always returns
    /// `n`.
    pub fn identity() -> Self {
        OffsetMap {
            entries: Vec::new(),
        }
    }

    /// Construct an `OffsetMap` from a pre-sorted list of drift entries.
    ///
    /// `entries` must be sorted in strictly ascending order by
    /// `normalised_pos`.  In debug builds this invariant is asserted.
    ///
    /// This is a `pub(crate)` constructor intended for the unit-test suite.
    #[cfg(test)]
    pub(crate) fn from_entries(entries: Vec<(u64, i64)>) -> Self {
        debug_assert!(
            entries.windows(2).all(|w| w[0].0 < w[1].0),
            "OffsetMap entries must be strictly sorted by normalised_pos"
        );
        OffsetMap { entries }
    }

    /// Append a drift entry in builder order.
    ///
    /// `normalised_pos` must be strictly greater than the last recorded entry's
    /// position (or any value if the map is empty).  In debug builds, the
    /// monotonicity invariant is asserted.
    ///
    /// This is a `pub(crate)` helper for the MA-25 scanner.
    pub(crate) fn push_entry(&mut self, normalised_pos: u64, cumulative_drift: i64) {
        debug_assert!(
            self.entries
                .last()
                .is_none_or(|&(last_p, _)| normalised_pos > last_p),
            "push_entry: normalised_pos must be strictly increasing"
        );
        self.entries.push((normalised_pos, cumulative_drift));
    }

    /// Convert a normalised byte offset to the corresponding source byte
    /// offset.
    ///
    /// Performs a binary search over the drift table to locate the entry whose
    /// `normalised_pos` is the largest value ≤ `normalised`, then adds that
    /// entry's cumulative drift to `normalised`.  When no entry applies (the
    /// normalised position precedes the first recorded drift), the drift is
    /// zero and `normalised` is returned unchanged.
    ///
    /// # Properties
    ///
    /// - `identity().to_source(n) == n` for all `n`.
    /// - `to_source` is monotonically non-decreasing.
    /// - The result is always ≥ `normalised` because cumulative drift is
    ///   non-negative (each CRLF adds exactly one to the drift).
    pub fn to_source(&self, normalised: u64) -> u64 {
        let drift = self.drift_at(normalised);
        // drift is always non-negative (cumulative count of extra CR bytes), so
        // the cast cannot overflow for any realistically-sized source file.
        (normalised as i64 + drift) as u64
    }

    /// Return the cumulative drift applicable at `normalised`.
    ///
    /// This is the drift of the last entry whose `normalised_pos ≤ normalised`,
    /// or zero if no such entry exists.
    #[inline]
    fn drift_at(&self, normalised: u64) -> i64 {
        // `partition_point` returns the first index where the predicate is
        // false, i.e. the first entry with normalised_pos > normalised.
        // The entry immediately before that index is the applicable one.
        let idx = self.entries.partition_point(|&(p, _)| p <= normalised);
        if idx == 0 { 0 } else { self.entries[idx - 1].1 }
    }
}

// ── MA-25: build_offset_map ───────────────────────────────────────────────────

/// Scan `branch[byte_range]` and produce an [`OffsetMap`] that records the
/// cumulative drift introduced by each CRLF → LF normalisation.
///
/// ## Normalisation rules
///
/// | Source sequence | Normalised | Drift change |
/// |---|---|---|
/// | `\r\n` (CRLF) | single `\n` | +1 |
/// | `\r` not followed by `\n` (lone CR) | single `\n` | 0 |
/// | `\n` (LF) | `\n` | 0 |
/// | any other byte | unchanged | 0 |
///
/// Lone CR is normalised to LF but does **not** change the cumulative drift
/// because it is a 1:1 byte substitution.
///
/// ## Drift entry placement
///
/// A drift entry is recorded at the normalised position *immediately after*
/// the replacement LF (see the [module-level documentation](self) for the
/// rationale).
///
/// ## I/O
///
/// The branch is read in 4096-byte chunks.  A `\r` that falls at the very end
/// of a chunk is handled correctly: the function peeks at the first byte of
/// the next chunk before deciding whether it constitutes a CRLF pair.
///
/// ## Errors
///
/// Returns [`std::io::Error`] if reading from the branch fails.
pub fn build_offset_map(
    branch: &dyn Branch,
    byte_range: Range<u64>,
) -> Result<OffsetMap, std::io::Error> {
    const CHUNK: usize = 4096;

    let mut map = OffsetMap::identity();

    let end = byte_range.end;
    let mut source_pos = byte_range.start;

    // Position in the virtual normalised stream.  Starts at 0 regardless of
    // where `byte_range` starts in the branch, so that the OffsetMap is
    // relative to the start of the byte_range.
    let mut normalised_pos: u64 = 0;
    let mut cumulative_drift: i64 = 0;

    // True when the last byte read from the previous chunk was a bare `\r`.
    // We cannot yet know whether it forms a CRLF until we see the next byte.
    let mut pending_cr = false;

    let mut buf = [0u8; CHUNK];

    while source_pos < end {
        let to_read = ((end - source_pos) as usize).min(CHUNK);
        let n = branch.read_at(source_pos, &mut buf[..to_read])?;
        if n == 0 {
            break;
        }

        let bytes = &buf[..n];
        let mut i = 0;

        // Resolve a `\r` that was left pending at the end of the previous chunk.
        if pending_cr {
            pending_cr = false;
            if bytes[i] == b'\n' {
                // The deferred `\r` and this `\n` together form a CRLF.
                // The `\r` was already counted as one normalised position, so
                // we just record the drift entry at the current normalised_pos
                // (which is one past the normalised LF) and skip the `\n`.
                cumulative_drift += 1;
                map.push_entry(normalised_pos, cumulative_drift);
                i += 1; // skip the `\n`
            }
            // else: the deferred `\r` was a lone CR — already counted in
            // normalised_pos, no drift change.
        }

        while i < n {
            match bytes[i] {
                b'\r' => {
                    // Consume the `\r` as one normalised byte unconditionally.
                    normalised_pos += 1;

                    if i + 1 < n {
                        // Can peek immediately within the same buffer.
                        if bytes[i + 1] == b'\n' {
                            // CRLF: record drift at normalised_pos (the position
                            // just past the normalised LF) and skip the `\n`.
                            cumulative_drift += 1;
                            map.push_entry(normalised_pos, cumulative_drift);
                            i += 2; // skip CR + LF
                        } else {
                            // Lone CR.
                            i += 1;
                        }
                    } else {
                        // `\r` is at the last byte of this chunk.  Defer the
                        // CRLF decision until the next chunk.
                        pending_cr = true;
                        i += 1;
                    }
                }
                _ => {
                    // `\n`, or any other byte: one normalised position, no drift.
                    normalised_pos += 1;
                    i += 1;
                }
            }
        }

        source_pos += n as u64;
    }

    Ok(map)
}
