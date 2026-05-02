// Copyright (c) 2026, Michael Grier

use crate::line_count::LineCount;

/// Events emitted by the line map as segments are scanned or invalidated.
///
/// Consumers receive these through an `mpsc::Sender<LineMapEvent>` passed to
/// the line map at construction time.  Events are always emitted in
/// chronological order within a single segment scan or invalidation call.
///
/// Changing any variant name or field layout is a breaking change for channel
/// consumers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineMapEvent {
    /// The current best estimate (or exact count) of total lines has changed.
    LineCountChanged {
        /// New count — may be an estimate (`exact: false`) or authoritative
        /// (`exact: true`).
        count: LineCount,
    },

    /// A contiguous range of lines now has exact terminator information.
    /// Both bounds are 0-based, inclusive.
    RegionExact {
        /// First line (0-based) known exactly.
        start_line: usize,
        /// Last line  (0-based) known exactly.
        end_line: usize,
    },

    /// All segment data at or after `from_line` has been discarded due to an
    /// edit.  Consumers must re-query line information beyond this point.
    MapInvalidated {
        /// First line (0-based) whose data is now stale.
        from_line: usize,
    },
}
