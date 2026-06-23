// Copyright (c) 2026, Michael Grier

//! Unit tests for `View`, `View::new`, and `View::apply` (MA-29).
//!
//! Every test is deterministic and uses in-memory branches only.

use std::sync::Arc;

use redwing::{Branch, make_thicket_from_bytes, materialize};

use crate::{
    offset_map::{OffsetMap, build_offset_map},
    view::View,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn branch_of(bytes: &[u8]) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.to_vec()).main()
}

/// Build a `View` over the entire content of `src_bytes`.
///
/// The offset map is built from `src_bytes` (as a branch), and the `bytes`
/// field is the LF-normalised form of `src_bytes`.  `byte_range_start` is 0.
fn view_of(src_bytes: &[u8]) -> View {
    let branch = branch_of(src_bytes);
    let map = build_offset_map(branch.as_ref(), 0..branch.byte_len()).unwrap();

    // Normalise manually for the `bytes` field: replace every \r\n → \n and
    // bare \r → \n.
    let mut normalised = Vec::with_capacity(src_bytes.len());
    let mut i = 0;
    while i < src_bytes.len() {
        match src_bytes[i] {
            b'\r' => {
                normalised.push(b'\n');
                if i + 1 < src_bytes.len() && src_bytes[i + 1] == b'\n' {
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

    View::new(normalised, map, branch, 0)
}

/// Materialise an `Arc<dyn Branch>` to a `Vec<u8>`.
fn mat(b: &Arc<dyn Branch>) -> Vec<u8> {
    materialize(b.as_ref()).unwrap()
}

// ── View::new / accessors ─────────────────────────────────────────────────────

/// `View::new` stores the bytes and offset map; accessors return the right values.
#[test]
fn new_stores_components() {
    let branch = branch_of(b"hello\r\nworld");
    let map = OffsetMap::identity();
    let view = View::new(b"hello\nworld".to_vec(), map, Arc::clone(&branch), 5);

    assert_eq!(view.bytes, b"hello\nworld");
    assert_eq!(view.byte_range_start(), 5);
    // branch() returns a handle to the same branch
    assert_eq!(view.branch().byte_len(), branch.byte_len());
}

// ── View::apply on all-LF source ─────────────────────────────────────────────

/// Apply at the very start of an LF source: replace first N bytes.
#[test]
fn apply_lf_source_replace_at_start() {
    let view = view_of(b"hello\nworld\n");

    // normalised range 0..5 covers "hello"; replace with "HI"
    let fork = view.apply(0..5, b"HI").unwrap();
    assert_eq!(mat(&fork), b"HI\nworld\n");
}

/// Apply at the middle of an LF source.
#[test]
fn apply_lf_source_replace_at_middle() {
    let view = view_of(b"abc\ndef\nghi\n");

    // normalised range 4..7 covers "def"; replace with "XY"
    let fork = view.apply(4..7, b"XY").unwrap();
    assert_eq!(mat(&fork), b"abc\nXY\nghi\n");
}

/// Apply at the very end of an LF source.
#[test]
fn apply_lf_source_replace_at_end() {
    let view = view_of(b"abc\ndef\n");

    // normalised range 4..8 covers "def\n"; replace with "ZZZ\n"
    let fork = view.apply(4..8, b"ZZZ\n").unwrap();
    assert_eq!(mat(&fork), b"abc\nZZZ\n");
}

// ── View::apply on CRLF source: coordinate translation ───────────────────────

/// On a CRLF source, a normalised range that falls before the first CRLF maps
/// to the correct source coordinates with no drift.
#[test]
fn apply_crlf_source_before_first_crlf() {
    // "hello\r\nworld\r\n"
    // normalised: "hello\nworld\n"   (12 bytes)
    // source:     "hello\r\nworld\r\n" (14 bytes)
    let view = view_of(b"hello\r\nworld\r\n");

    // Replace normalised "hello" (0..5) in the source
    let fork = view.apply(0..5, b"HI").unwrap();
    // source had "hello\r\nworld\r\n"; replacing source bytes 0..5 gives "HI\r\nworld\r\n"
    assert_eq!(mat(&fork), b"HI\r\nworld\r\n");
}

/// On a CRLF source, a normalised range that starts after the first CRLF is
/// translated correctly (drift = 1).
#[test]
fn apply_crlf_source_after_first_crlf() {
    // "hello\r\nworld\r\n"
    // normalised: "hello\nworld\n" (12 bytes, 0-indexed)
    // normalised[6..11] = "world"; in source drift at pos 6 = 1, so source start = 7
    let view = view_of(b"hello\r\nworld\r\n");

    // source[7..12] = "world"; replace with "rust"
    let fork = view.apply(6..11, b"rust").unwrap();
    assert_eq!(mat(&fork), b"hello\r\nrust\r\n");
}

/// A range spanning across a CRLF maps its start and end correctly.
#[test]
fn apply_crlf_source_range_spanning_crlf() {
    // "AB\r\nCD"
    // normalised: "AB\nCD" (5 bytes)
    // source:     "AB\r\nCD" (6 bytes)
    // normalised 0..2 = "AB"; normalised 2..3 = "\n" (repl for \r); normalised 3..5 = "CD"
    // Replace normalised 0..5 ("AB\nCD") with "X" → source 0..6 deleted → "X"
    let view = view_of(b"AB\r\nCD");

    let fork = view.apply(0..5, b"X").unwrap();
    assert_eq!(mat(&fork), b"X");
}

// ── replacement shorter / same / longer than deleted region ──────────────────

/// Replacement shorter than the deleted source region.
#[test]
fn apply_replacement_shorter_than_source() {
    let view = view_of(b"aaaaa\nbbbbb\n");

    // "aaaaa" is 5 bytes; replace with "X" (1 byte)
    let fork = view.apply(0..5, b"X").unwrap();
    assert_eq!(mat(&fork), b"X\nbbbbb\n");
}

/// Replacement same length as the deleted source region.
#[test]
fn apply_replacement_same_length_as_source() {
    let view = view_of(b"hello\nworld\n");

    // "hello" (5 bytes) → "HELLO" (5 bytes)
    let fork = view.apply(0..5, b"HELLO").unwrap();
    assert_eq!(mat(&fork), b"HELLO\nworld\n");
}

/// Replacement longer than the deleted source region.
#[test]
fn apply_replacement_longer_than_source() {
    let view = view_of(b"hi\nworld\n");

    // "hi" (2 bytes) → "greetings" (9 bytes)
    let fork = view.apply(0..2, b"greetings").unwrap();
    assert_eq!(mat(&fork), b"greetings\nworld\n");
}

// ── empty replacement ─────────────────────────────────────────────────────────

/// Empty replacement deletes the selected source bytes.
#[test]
fn apply_empty_replacement_deletes_bytes() {
    let view = view_of(b"abcdef\n");

    // delete "abc" (normalised 0..3)
    let fork = view.apply(0..3, b"").unwrap();
    assert_eq!(mat(&fork), b"def\n");
}

/// Empty range with empty replacement is a no-op.
#[test]
fn apply_empty_range_empty_replacement_noop() {
    let src = b"unchanged\n";
    let view = view_of(src);

    let fork = view.apply(5..5, b"").unwrap();
    assert_eq!(mat(&fork), src);
}

// ── view spanning full branch ─────────────────────────────────────────────────

/// View spanning the full branch: apply touches bytes at both edges.
#[test]
fn apply_full_branch_view() {
    let view = view_of(b"start\r\nend");

    // full normalised range 0..9 → replace everything
    let fork = view.apply(0..9, b"NEW").unwrap();
    assert_eq!(mat(&fork), b"NEW");
}

// ── CRLF → correct source bytes materialised ─────────────────────────────────

/// Materialising the fork of a CRLF branch after an apply produces the
/// expected source bytes including any untouched CRLFs.
#[test]
fn apply_crlf_fork_materialises_correctly() {
    // "line1\r\nline2\r\nline3\r\n"
    let view = view_of(b"line1\r\nline2\r\nline3\r\n");

    // normalised: "line1\nline2\nline3\n" (18 bytes)
    // Replace "line2" at normalised 6..11 with "REPLACED"
    // drift at 6 = 1 → source start = 7; drift at 11 = 1 → source end = 12
    // source[7..12] = "line2"
    let fork = view.apply(6..11, b"REPLACED").unwrap();
    assert_eq!(mat(&fork), b"line1\r\nREPLACED\r\nline3\r\n");
}

// ── multi-apply: sequential applies in original (view) coordinates ────────────

/// Multiple independent applies in the *original* normalised coordinates each
/// produce a correct independent fork.  (Chained applies would require
/// coordinate remapping which is out of scope here.)
#[test]
fn apply_multiple_independent_forks_correct() {
    let view = view_of(b"alpha\nbeta\ngamma\n");

    let fork1 = view.apply(0..5, b"ALPHA").unwrap();
    let fork2 = view.apply(6..10, b"BETA").unwrap();
    let fork3 = view.apply(11..16, b"GAMMA").unwrap();

    assert_eq!(mat(&fork1), b"ALPHA\nbeta\ngamma\n");
    assert_eq!(mat(&fork2), b"alpha\nBETA\ngamma\n");
    assert_eq!(mat(&fork3), b"alpha\nbeta\nGAMMA\n");
}

// ── View::branch() and byte_range_start() ─────────────────────────────────────

/// The `branch()` accessor returns a handle that reads the same bytes as the
/// original branch.
#[test]
fn branch_accessor_reads_original_bytes() {
    let src = b"original content\r\n";
    let view = view_of(src);
    assert_eq!(mat(&view.branch()), src as &[u8]);
}

/// The `byte_range_start()` accessor returns 0 for a full-branch view.
#[test]
fn byte_range_start_is_zero_for_full_view() {
    let view = view_of(b"test\r\ndata\r\n");
    assert_eq!(view.byte_range_start(), 0);
}

/// A `View` constructed with a non-zero `byte_range_start` translates
/// normalised positions to the correct branch-absolute source positions.
#[test]
fn apply_with_nonzero_byte_range_start() {
    // Branch: "PREFIX\r\nhello\r\nSUFFIX" (21 bytes)
    // We create a view that covers only the "hello\r\n" portion starting at
    // source offset 8 (after "PREFIX\r\n").
    let src = b"PREFIX\r\nhello\r\nSUFFIX";
    let branch = branch_of(src);

    // Scanned range: 8..15 = "hello\r\n"
    let range_start: u64 = 8;
    let range_end: u64 = 15;
    let map = build_offset_map(branch.as_ref(), range_start..range_end).unwrap();

    // Normalised bytes for "hello\r\n" → "hello\n" (6 bytes)
    let normalised = b"hello\n".to_vec();

    let view = View::new(normalised, map, Arc::clone(&branch), range_start);

    // Apply: replace normalised "hello" (0..5) with "world"
    // drift at 0 = 0 → range-relative source 0 → branch-absolute 8 + 0 = 8
    // drift at 5 = 0 → range-relative source 5 → branch-absolute 8 + 5 = 13
    // source[8..13] = "hello"; replace with "world"
    let fork = view.apply(0..5, b"world").unwrap();
    assert_eq!(mat(&fork), b"PREFIX\r\nworld\r\nSUFFIX");
}

/// Empty replacement on CRLF source deletes the correct source bytes.
#[test]
fn apply_empty_replacement_crlf_source() {
    // "AB\r\nCD\r\n" normalised = "AB\nCD\n"
    // Delete normalised "AB" (0..2) → source 0..2 deleted → "\r\nCD\r\n"
    let view = view_of(b"AB\r\nCD\r\n");
    let fork = view.apply(0..2, b"").unwrap();
    assert_eq!(mat(&fork), b"\r\nCD\r\n");
}

/// Apply with a replacement that itself contains CRLF bytes: the CRLF bytes
/// are stored verbatim in the fork (no renormalisation).
#[test]
fn apply_replacement_containing_crlf_stored_verbatim() {
    let view = view_of(b"line1\nline2\n");
    // Replace "line1" with a CRLF-terminated version
    let fork = view.apply(0..5, b"LINE1\r").unwrap();
    assert_eq!(mat(&fork), b"LINE1\r\nline2\n");
}

// ── View::apply bounds validation ────────────────────────────────────────────

/// Inverted range (end < start) must return InvalidInput, not panic.
#[test]
#[allow(clippy::reversed_empty_ranges)] // the reversed range is the point of this test
fn apply_inverted_range_returns_error() {
    let view = view_of(b"hello\n");
    // start (4) > end (2)
    let result = view.apply(4..2, b"x");
    assert!(result.is_err());
    let err = result.err().expect("expected Err");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "expected InvalidInput, got {err}"
    );
}

/// End offset past the view length must return InvalidInput.
#[test]
fn apply_end_past_view_length_returns_error() {
    let view = view_of(b"hello\n");
    // normalised length is 6; 7 is past the end
    let result = view.apply(0..7, b"x");
    assert!(result.is_err());
    let err = result.err().expect("expected Err");
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::InvalidInput,
        "expected InvalidInput, got {err}"
    );
}

/// Start offset equal to end offset (empty range) with end == view length is valid.
#[test]
fn apply_empty_range_at_eof_is_valid() {
    let view = view_of(b"hello\n");
    // normalised length is 6; inserting at position 6 (after last byte) is valid
    let fork = view.apply(6..6, b" world").unwrap();
    assert_eq!(mat(&fork), b"hello\n world");
}

/// Start offset equal to end offset (empty range) at view boundary is valid.
#[test]
fn apply_end_exactly_at_view_length_is_valid() {
    let view = view_of(b"abc");
    // normalised length is 3; 3..3 is an empty append at the end
    let fork = view.apply(3..3, b"!").unwrap();
    assert_eq!(mat(&fork), b"abc!");
}
