// Copyright (c) 2026, Michael Grier

//! Unit tests for `OffsetMap` and `build_offset_map` (MA-28).
//!
//! Every test is deterministic and requires no I/O beyond in-memory buffers.

use std::sync::Arc;

use redwing::{make_thicket_from_bytes, Branch};

use crate::offset_map::{build_offset_map, OffsetMap};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Return an `Arc<dyn Branch>` backed by an in-memory copy of `bytes`.
fn branch_of(bytes: &[u8]) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.to_vec()).main()
}

/// Build an `OffsetMap` over the entire byte range of a branch created from
/// `bytes`.
fn build(bytes: &[u8]) -> OffsetMap {
    let branch = branch_of(bytes);
    build_offset_map(branch.as_ref(), 0..branch.byte_len()).unwrap()
}

// ── OffsetMap struct unit tests ───────────────────────────────────────────────

/// An identity map has zero drift for every normalised position.
#[test]
fn identity_to_source_is_identity() {
    let map = OffsetMap::identity();
    for n in [0u64, 1, 100, u64::MAX / 2] {
        assert_eq!(map.to_source(n), n, "identity map must return n for all n");
    }
}

/// `from_entries` with one entry gives correct drift on both sides of the
/// recorded position.
#[test]
fn from_entries_single_entry_boundary() {
    // drift of 2 kicks in at normalised position 10
    let map = OffsetMap::from_entries(vec![(10, 2)]);

    // before the entry: drift = 0
    assert_eq!(map.to_source(9), 9);

    // at the entry exactly: drift = 2
    assert_eq!(map.to_source(10), 12);

    // after the entry: drift = 2
    assert_eq!(map.to_source(20), 22);
}

/// `from_entries` with multiple entries produces the correct drift in every
/// interval between successive entries.
#[test]
fn from_entries_multiple_entries() {
    let map = OffsetMap::from_entries(vec![(5, 1), (10, 2), (20, 5)]);

    // before first entry
    assert_eq!(map.to_source(4), 4);

    // at first entry
    assert_eq!(map.to_source(5), 6); // 5 + 1

    // between first and second
    assert_eq!(map.to_source(7), 8); // 7 + 1

    // at second entry
    assert_eq!(map.to_source(10), 12); // 10 + 2

    // between second and third
    assert_eq!(map.to_source(15), 17); // 15 + 2

    // at third entry
    assert_eq!(map.to_source(20), 25); // 20 + 5

    // after third entry
    assert_eq!(map.to_source(30), 35); // 30 + 5
}

// ── build_offset_map: normal cases ───────────────────────────────────────────

/// A pure-LF source produces an identity map (no CRLF sequences).
#[test]
fn all_lf_no_drift() {
    let src = b"line one\nline two\nline three\n";
    let map = build(src);

    for n in 0..src.len() as u64 {
        assert_eq!(
            map.to_source(n),
            n,
            "pure-LF source must have zero drift at {n}"
        );
    }
}

/// All-CRLF source: every CRLF adds 1 to the cumulative drift.  After N
/// CRLFs the drift at the normalised end-of-stream equals N.
#[test]
fn all_crlf_drift_equals_crlf_count() {
    // 3 CRLF sequences → source length 6, normalised length 3
    let src = b"\r\n\r\n\r\n";
    let map = build(src);

    // Normalised positions 0, 1, 2 each map to the CR that starts each CRLF.
    // drift entry placement: entry at normalised_pos AFTER the replacement LF.
    // After first CRLF:  entry (1, 1) — so to_source(0) = 0
    // After second CRLF: entry (2, 2) — so to_source(1) = 2
    // After third CRLF:  entry (3, 3) — so to_source(2) = 4
    assert_eq!(map.to_source(0), 0); // first \r
    assert_eq!(map.to_source(1), 2); // second \r
    assert_eq!(map.to_source(2), 4); // third \r
                                     // "end" position: normalised length is 3, source length is 6
    assert_eq!(map.to_source(3), 6);
}

/// Mixed CRLF and LF: only the CRLF sequences contribute drift.
#[test]
fn mixed_crlf_and_lf() {
    // source: "a\r\nb\nc\r\n"  →  8 source bytes
    // normalised: "a\nb\nc\n"  →  7 bytes
    // CRLF 1 ends at source offset 2 (the \n), normalised offset 1 (=\r mapped)
    //   entry after: (2, 1)
    // CRLF 2 ends at source offset 8, normalised offset 6 (=\r mapped)
    //   entry after: (7, 2)
    let src = b"a\r\nb\nc\r\n";
    let map = build(src);

    // 'a' at normalised 0 → source 0
    assert_eq!(map.to_source(0), 0);
    // the LF that replaced CRLF 1, at normalised 1 → source 1 (the \r)
    assert_eq!(map.to_source(1), 1);
    // 'b' at normalised 2 → source 3 (drift 1)
    assert_eq!(map.to_source(2), 3);
    // bare '\n' at normalised 3 → source 4 (drift 1)
    assert_eq!(map.to_source(3), 4);
    // 'c' at normalised 4 → source 5 (drift 1)
    assert_eq!(map.to_source(4), 5);
    // the LF that replaced CRLF 2, at normalised 5 → source 6 (the \r)
    assert_eq!(map.to_source(5), 6);
    // end-of-stream: normalised 7 → source 9 (drift 2)?
    // Wait: source is 9 bytes (indices 0..8), normalised is 7 bytes.
    // But drift at normalised 7 should be 2, so to_source(7) = 9 ✓
    assert_eq!(map.to_source(7), 9);
}

/// Lone CR (not followed by LF) is normalised 1:1 — no drift.
#[test]
fn lone_cr_no_drift() {
    // "A\rB\rC" — 5 bytes, two lone CRs, zero drift
    let src = b"A\rB\rC";
    let map = build(src);

    for n in 0..src.len() as u64 {
        assert_eq!(
            map.to_source(n),
            n,
            "lone-CR source must have zero drift at {n}"
        );
    }
}

/// Mixed lone-CR and CRLF: only CRLF contributes drift.
#[test]
fn mixed_lone_cr_and_crlf() {
    // "\rA\r\nB"  →  source: [\r, A, \r, \n, B] (5 bytes)
    // normalised: [\n, A, \n, B] (4 bytes)
    // lone \r at source 0 → normalised 0, no drift
    // \r at source 2 + \n at source 3 → CRLF; normalised LF at position 2
    //   entry after normalised LF: (3, 1)
    let src = b"\rA\r\nB";
    let map = build(src);

    // normalised 0 (\n replacing lone \r) → source 0
    assert_eq!(map.to_source(0), 0);
    // normalised 1 (A) → source 1
    assert_eq!(map.to_source(1), 1);
    // normalised 2 (\n replacing CRLF \r) → source 2 (the \r)
    assert_eq!(map.to_source(2), 2);
    // normalised 3 (B) → source 4 (drift 1)
    assert_eq!(map.to_source(3), 4);
}

/// CRLF at the very beginning of the range (byte 0, 1).
#[test]
fn crlf_at_byte_zero() {
    // "\r\nX" → source 3 bytes, normalised 2 bytes
    // entry: (1, 1)
    let src = b"\r\nX";
    let map = build(src);

    // normalised 0 (the LF replacing \r) → source 0 (the \r)
    assert_eq!(map.to_source(0), 0);
    // normalised 1 (X) → source 2 (drift 1)
    assert_eq!(map.to_source(1), 2);
    // end: normalised 2 → source 3 (drift 1)
    assert_eq!(map.to_source(2), 3);
}

/// CRLF at the very end of the range (last two bytes).
#[test]
fn crlf_at_last_two_bytes() {
    // "X\r\n" → source 3 bytes, normalised 2 bytes
    // entry: (2, 1)  [normalised 0=X, normalised 1=\n-repl-for-\r, entry at 2]
    let src = b"X\r\n";
    let map = build(src);

    // 'X' at normalised 0 → source 0
    assert_eq!(map.to_source(0), 0);
    // '\r' at normalised 1 → source 1 (no drift yet)
    assert_eq!(map.to_source(1), 1);
    // end: normalised 2 → source 3 (drift 1)
    assert_eq!(map.to_source(2), 3);
}

/// `to_source` returns correct results when called at exactly the position of
/// a recorded drift entry.
#[test]
fn binary_search_exact_hit_on_entry_position() {
    // Two CRLFs: "\r\nA\r\nB"
    // source: [\r,\n,A,\r,\n,B] (6 bytes)
    // normalised: [\n,A,\n,B] (4 bytes)
    // entries: (1, 1), (3, 2)
    let src = b"\r\nA\r\nB";
    let map = build(src);

    // exact hit at entry (1, 1): to_source(1) = 1 + 1 = 2 ('A')
    assert_eq!(map.to_source(1), 2);
    // exact hit at entry (3, 2): to_source(3) = 3 + 2 = 5 ('B')
    assert_eq!(map.to_source(3), 5);
}

/// `to_source` between two recorded entries uses the earlier entry's drift.
#[test]
fn binary_search_between_entries() {
    // Four CRLFs separated by 3 bytes each:
    // "\r\nAAA\r\nAAA\r\nAAA\r\n"
    // CRLFs contribute entries at normalised positions 1, 5, 9, 13.
    let src = b"\r\nAAA\r\nAAA\r\nAAA\r\n";
    let map = build(src);

    // between entry (1,1) and (5,2): positions 2,3,4 all have drift 1
    assert_eq!(map.to_source(2), 3); // 2 + 1
    assert_eq!(map.to_source(3), 4); // 3 + 1
    assert_eq!(map.to_source(4), 5); // 4 + 1

    // between entry (5,2) and (9,3): positions 6,7,8 have drift 2
    assert_eq!(map.to_source(6), 8); // 6 + 2
    assert_eq!(map.to_source(7), 9); // 7 + 2
    assert_eq!(map.to_source(8), 10); // 8 + 2
}

/// Each successive CRLF increases the cumulative drift by exactly 1.
#[test]
fn drift_accumulation_is_strictly_increasing() {
    // 5 CRLFs in a row
    let src = b"\r\n\r\n\r\n\r\n\r\n";
    let map = build(src);

    // to_source at each normalised position (0..=5):
    // 0→0, 1→2, 2→4, 3→6, 4→8, 5→10
    for (n, expected_source) in (0u64..=5).zip([0u64, 2, 4, 6, 8, 10]) {
        assert_eq!(
            map.to_source(n),
            expected_source,
            "at normalised {n} expected source {expected_source}"
        );
    }
}

/// An empty byte range produces an identity map.
#[test]
fn empty_range_produces_identity_map() {
    let branch = branch_of(b"hello\r\nworld");
    let map = build_offset_map(branch.as_ref(), 5..5).unwrap();

    // No bytes scanned → no entries → identity
    assert_eq!(map.to_source(0), 0);
    assert_eq!(map.to_source(100), 100);
}

/// A single LF byte produces an identity map (no CRLF).
#[test]
fn single_byte_lf_is_identity() {
    let map = build(b"\n");
    assert_eq!(map.to_source(0), 0);
}

/// A single CR byte (lone CR, no following LF) produces an identity map.
#[test]
fn single_byte_lone_cr_is_identity() {
    let map = build(b"\r");
    assert_eq!(map.to_source(0), 0);
}

/// A single CRLF pair produces a map with one entry: drift 1 at position 1.
#[test]
fn single_crlf_pair() {
    let map = build(b"\r\n");
    // normalised 0 (the LF) → source 0 (the \r), no drift yet
    assert_eq!(map.to_source(0), 0);
    // end position: normalised 1 → source 2 (drift 1)
    assert_eq!(map.to_source(1), 2);
}

/// CRLF that straddles a 4096-byte chunk boundary is handled correctly.
///
/// The buffer is constructed so that the `\r` falls at source offset 4095
/// (the last byte of the first 4096-byte chunk) and the `\n` falls at
/// source offset 4096.
#[test]
fn crlf_at_chunk_boundary() {
    const CHUNK: usize = 4096;
    // 4095 filler bytes, then \r\n, then one more filler byte
    let mut src = vec![b'X'; CHUNK - 1];
    src.push(b'\r');
    src.push(b'\n');
    src.push(b'Z');

    let map = build(&src);

    // All filler bytes up to (but not including) the \r: zero drift
    for n in 0..(CHUNK as u64 - 1) {
        assert_eq!(
            map.to_source(n),
            n,
            "no drift before the CRLF at normalised {n}"
        );
    }

    // The replacement LF for the CRLF is at normalised CHUNK-1 (= 4095).
    // to_source(4095) must map to source 4095 (the \r).
    assert_eq!(map.to_source(CHUNK as u64 - 1), CHUNK as u64 - 1);

    // The 'Z' after the CRLF is at normalised CHUNK (= 4096) with drift 1.
    assert_eq!(map.to_source(CHUNK as u64), CHUNK as u64 + 1);
}

/// `build_offset_map` respects a sub-range of the branch.
///
/// The offset map returned covers only the specified byte range; positions
/// within the map are relative to the start of the range (not branch-absolute).
#[test]
fn sub_range_drift_is_range_relative() {
    // branch: "prefix\r\nsuffix"
    // byte range: 6..10 covers "\r\nsu"
    // normalised positions are relative to offset 6:
    //   0 → \r, 1 → \n skipped, 2 → s, 3 → u
    //   entry: (1, 1)
    let src = b"prefix\r\nsuffix";
    let branch = branch_of(src);
    let map = build_offset_map(branch.as_ref(), 6..10).unwrap();

    // relative normalised 0 → the \r → branch offset 6
    // (range-relative source 0; caller adds byte_range_start=6)
    assert_eq!(map.to_source(0), 0);
    // relative normalised 1 → 's' → range-relative source 2 (drift 1)
    assert_eq!(map.to_source(1), 2);
}

/// A source with no line endings at all (arbitrary binary-ish data) stays
/// identity.
#[test]
fn no_line_endings_is_identity() {
    let src: Vec<u8> = (0u8..=127u8)
        .filter(|&b| b != b'\r' && b != b'\n')
        .collect();
    let map = build(&src);
    for n in 0..src.len() as u64 {
        assert_eq!(map.to_source(n), n);
    }
}

/// Large number of CRLFs produces correct accumulated drift at end.
#[test]
fn large_crlf_count_drift_at_end() {
    const N: u64 = 1000;
    let mut src = Vec::with_capacity((N * 2) as usize);
    for _ in 0..N {
        src.push(b'\r');
        src.push(b'\n');
    }
    let map = build(&src);
    // normalised length = N, source length = 2*N
    // to_source(N) = N + N = 2*N
    assert_eq!(map.to_source(N), 2 * N);
    // to_source(0) = 0 (first \r)
    assert_eq!(map.to_source(0), 0);
    // to_source(N-1) = (N-1) + (N-1) = 2*(N-1), i.e. the last \r
    assert_eq!(map.to_source(N - 1), 2 * (N - 1));
}

/// Interleaved lone CRs and CRLFs: drift counts only CRLFs.
#[test]
fn interleaved_lone_cr_and_crlf_drift() {
    // "\r" lone — no drift
    // "\r\n" crlf — drift + 1
    // "\r" lone — no drift
    // "\r\n" crlf — drift + 1
    // source bytes: [\r, \r, \n, \r, \r, \n] = 6
    // normalised:   [\n, \n,     \n, \n    ] = 4 (CRLFs each collapse)
    //   Wait — that's not right. Let me re-trace:
    //   source: \r \r \n \r \r \n
    //   i=0: \r; peek i+1=\r (not \n) → lone CR, normalised_pos=1, i=1
    //   i=1: \r; peek i+1=\n → CRLF, entry(2,1), i=3; normalised_pos=2
    //   i=3: \r; peek i+1=\r (not \n) → lone CR, normalised_pos=3, i=4
    //   i=4: \r; peek i+1=\n → CRLF, entry(4,2), i=6; normalised_pos=4
    let src = b"\r\r\n\r\r\n";
    let map = build(src);

    // normalised pos 0 (lone \r) → source 0
    assert_eq!(map.to_source(0), 0);
    // normalised pos 1 (CRLF \r) → source 1 (no drift yet, entry is at pos 2)
    assert_eq!(map.to_source(1), 1);
    // normalised pos 2 (lone \r) → source 3 (drift 1)
    assert_eq!(map.to_source(2), 3);
    // normalised pos 3 (CRLF \r) → source 4 (drift 1, entry at pos 4)
    assert_eq!(map.to_source(3), 4);
    // end: normalised 4 → source 6 (drift 2)
    assert_eq!(map.to_source(4), 6);
}
