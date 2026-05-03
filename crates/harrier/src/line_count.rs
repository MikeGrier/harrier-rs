// Copyright (c) 2026, Michael Grier

use std::fmt;

/// An estimate or exact count of lines in a branch.
///
/// `exact: false` means the value was extrapolated from a partially-scanned
/// segment map; it should be treated as an approximation.  Display renders it
/// with a leading `~` to signal that it is not guaranteed.
///
/// `exact: true` means the whole branch has been scanned and the value is
/// authoritative.
///
/// Changing the discriminant meaning of the `exact` flag is a breaking change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LineCount {
    /// The line count (exact or estimated).
    pub value: usize,
    /// `true` when the full branch has been scanned.
    pub exact: bool,
}

impl LineCount {
    /// Construct an exact count.
    pub fn exact(value: usize) -> Self {
        Self { value, exact: true }
    }

    /// Construct an estimated (extrapolated) count.
    pub fn estimate(value: usize) -> Self {
        Self {
            value,
            exact: false,
        }
    }
}

impl fmt::Display for LineCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.exact {
            write!(f, "{}", self.value)
        } else {
            write!(f, "~{}", self.value)
        }
    }
}
