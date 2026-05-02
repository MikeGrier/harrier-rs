// Copyright (c) 2026, Michael Grier

//! MA-IT-3: Milestone 3 integration test — editor simulation.
//!
//! Opens both an LF file and a CRLF file and performs the following sequence
//! on each:
//!
//!   1. Navigate to line N via `Buffer::line_offset` and verify the byte offset.
//!   2. Insert a blank line before line N via `Buffer::insert_line`.
//!   3. Verify the inserted line is visible and subsequent lines shifted down.
//!   4. Delete the blank line via `Buffer::apply_edit` with empty replacement.
//!   5. Verify the buffer is back to its original shape.
//!   6. Apply a multi-line pattern replacement via `view_range` + `apply_edit`.
//!   7. Materialise the edited branch and compare byte-for-byte with the
//!      expected output, verifying that the native line terminators are
//!      preserved throughout.

use std::sync::Arc;

use encoding_rs::UTF_8;
use weaver::{
    buffer::Buffer,
    encoding::{BomPolicy, SourceConfig},
    source::Source,
};
use redwing::{make_thicket_from_bytes, materialize, Branch};

// ── helpers ───────────────────────────────────────────────────────────────────

fn branch_of(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

fn make_buffer(bytes: impl Into<Vec<u8>>) -> Buffer {
    let b = branch_of(bytes);
    Source::new(
        b,
        SourceConfig {
            encoding_hint: Some(UTF_8),
            bom_policy: BomPolicy::Ignore,
            ..SourceConfig::default()
        },
    )
    .expect("Source::new")
    .as_buffer()
    .expect("as_buffer")
}

// ── LF baseline test data ─────────────────────────────────────────────────────
//
// 5-line LF file — each line is "word\n":
//
//   Line 0: "alpha\n"    bytes 0..6
//   Line 1: "beta\n"     bytes 6..11
//   Line 2: "gamma\n"    bytes 11..17
//   Line 3: "delta\n"    bytes 17..23
//   Line 4: "epsilon\n"  bytes 23..31
//
const LF_SOURCE: &[u8] = b"alpha\nbeta\ngamma\ndelta\nepsilon\n";

// ── CRLF baseline test data ───────────────────────────────────────────────────
//
// Same 5-line structure with CRLF terminators:
//
//   Line 0: "alpha\r\n"    bytes 0..7
//   Line 1: "beta\r\n"     bytes 7..13
//   Line 2: "gamma\r\n"    bytes 13..20
//   Line 3: "delta\r\n"    bytes 20..27
//   Line 4: "epsilon\r\n"  bytes 27..36
//
const CRLF_SOURCE: &[u8] = b"alpha\r\nbeta\r\ngamma\r\ndelta\r\nepsilon\r\n";

// ── MA-IT-3: LF file — full editor simulation ─────────────────────────────────

#[test]
fn editor_simulation_lf() {
    let mut buf = make_buffer(LF_SOURCE.to_vec());

    // ── Step 1: navigate to line 2 ────────────────────────────────────────────
    // "alpha\n" (6) + "beta\n" (5) = 11
    assert_eq!(buf.line_offset(0).unwrap(), 0, "line 0 at 0");
    assert_eq!(buf.line_offset(1).unwrap(), 6, "line 1 at 6");
    assert_eq!(buf.line_offset(2).unwrap(), 11, "line 2 at 11");
    assert_eq!(buf.line_offset(3).unwrap(), 17, "line 3 at 17");
    assert_eq!(buf.line_offset(4).unwrap(), 23, "line 4 at 23");

    // ── Step 2: insert a blank line before line 2 ─────────────────────────────
    // This inserts "\n" at byte 11 (the start of "gamma\n").
    buf.insert_line(2).unwrap();
    // Branch is now: "alpha\nbeta\n\ngamma\ndelta\nepsilon\n" (32 bytes)
    assert_eq!(buf.line_offset(2).unwrap(), 11, "blank line 2 starts at 11");
    assert_eq!(
        buf.line_offset(3).unwrap(),
        12,
        "gamma shifted to line 3 at 12"
    );

    // ── Step 3: verify shifted content ───────────────────────────────────────
    let view2 = buf.line_content(2).unwrap();
    assert_eq!(view2.bytes, b"\n", "blank line content is just LF");
    let view3 = buf.line_content(3).unwrap();
    assert_eq!(view3.bytes, b"gamma\n", "gamma is now on line 3");

    // ── Step 4: delete the blank line (line 2) via apply_edit + empty replacement
    {
        let blank_view = buf.line_content(2).unwrap();
        let blank_len = blank_view.bytes.len() as u64;
        buf.apply_edit(&blank_view, 0..blank_len, &[]).unwrap();
    }
    // Branch is now back to the original shape.
    assert_eq!(buf.line_offset(2).unwrap(), 11, "line 2 restored to 11");
    let view2_restored = buf.line_content(2).unwrap();
    assert_eq!(view2_restored.bytes, b"gamma\n", "gamma back on line 2");

    // ── Step 5: multi-line replacement via view_range + apply_edit ────────────
    // Replace the normalised two-line span "beta\ngamma\n" (at normalised pos
    // 6..17 in the original 31-byte branch) with "BETA\nGAMMA\n".
    let total = buf.branch().byte_len();
    let full_view = buf.view_range(0..total).unwrap();

    // Locate the two-line pattern in the normalised view.
    let pattern = b"beta\ngamma\n";
    let pos = full_view
        .bytes
        .windows(pattern.len())
        .position(|w| w == pattern)
        .expect("pattern must be present in normalised view") as u64;
    let end_pos = pos + pattern.len() as u64;

    buf.apply_edit(&full_view, pos..end_pos, b"BETA\nGAMMA\n")
        .unwrap();

    // ── Step 6: materialise and compare byte-for-byte ─────────────────────────
    let result = materialize(buf.branch().as_ref()).unwrap();
    // LF file → terminators stay LF.
    assert_eq!(
        result,
        b"alpha\nBETA\nGAMMA\ndelta\nepsilon\n".as_ref(),
        "LF file: replacement preserved LF terminators"
    );
}

// ── MA-IT-3: CRLF file — full editor simulation ───────────────────────────────

#[test]
fn editor_simulation_crlf() {
    let mut buf = make_buffer(CRLF_SOURCE.to_vec());

    // ── Step 1: navigate to line 2 ────────────────────────────────────────────
    // "alpha\r\n" (7) + "beta\r\n" (6) = 13
    assert_eq!(buf.line_offset(0).unwrap(), 0, "line 0 at 0");
    assert_eq!(buf.line_offset(1).unwrap(), 7, "line 1 at 7");
    assert_eq!(buf.line_offset(2).unwrap(), 13, "line 2 at 13");
    assert_eq!(buf.line_offset(3).unwrap(), 20, "line 3 at 20");
    assert_eq!(buf.line_offset(4).unwrap(), 27, "line 4 at 27");

    // ── Step 2: insert a blank CRLF line before line 2 ───────────────────────
    // Inserts "\r\n" at byte 13.
    buf.insert_line(2).unwrap();
    // "alpha\r\nbeta\r\n\r\ngamma\r\ndelta\r\nepsilon\r\n" (38 bytes)
    assert_eq!(
        buf.line_offset(2).unwrap(),
        13,
        "blank CRLF line 2 starts at 13"
    );
    assert_eq!(
        buf.line_offset(3).unwrap(),
        15,
        "gamma shifted to line 3 at 15"
    );

    // ── Step 3: verify content ────────────────────────────────────────────────
    let view2 = buf.line_content(2).unwrap();
    // CRLF "\r\n" normalises to "\n"
    assert_eq!(view2.bytes, b"\n", "blank CRLF line normalises to LF");
    let view3 = buf.line_content(3).unwrap();
    assert_eq!(
        view3.bytes, b"gamma\n",
        "gamma still normalises to gamma\\n"
    );

    // ── Step 4: delete the blank line (line 2) ───────────────────────────────
    {
        let blank_view = buf.line_content(2).unwrap();
        let blank_len = blank_view.bytes.len() as u64;
        buf.apply_edit(&blank_view, 0..blank_len, &[]).unwrap();
    }
    assert_eq!(buf.line_offset(2).unwrap(), 13, "gamma back at 13");
    let view2_restored = buf.line_content(2).unwrap();
    assert_eq!(view2_restored.bytes, b"gamma\n", "gamma restored");

    // ── Step 5: multi-line replacement ───────────────────────────────────────
    // Normalised view of the full CRLF branch: "alpha\nbeta\ngamma\ndelta\nepsilon\n"
    // Pattern "beta\ngamma\n" is in normalised space.
    // apply_edit translates back to source CRLF coordinates automatically.
    let total = buf.branch().byte_len();
    let full_view = buf.view_range(0..total).unwrap();

    let pattern = b"beta\ngamma\n";
    let pos = full_view
        .bytes
        .windows(pattern.len())
        .position(|w| w == pattern)
        .expect("pattern in normalised CRLF view") as u64;
    let end_pos = pos + pattern.len() as u64;

    // The replacement is in normalised (LF-only) bytes; apply_edit splices
    // raw (un-normalised) bytes directly into the source branch.  To preserve
    // CRLF terminators the replacement must already use CRLF.
    buf.apply_edit(&full_view, pos..end_pos, b"BETA\r\nGAMMA\r\n")
        .unwrap();

    // ── Step 6: materialise and compare ──────────────────────────────────────
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(
        result,
        b"alpha\r\nBETA\r\nGAMMA\r\ndelta\r\nepsilon\r\n".as_ref(),
        "CRLF file: terminators preserved"
    );
}

// ── split_line: LF file ───────────────────────────────────────────────────────

/// Split "beta\n" at column 2 (after "be") → "be\nta\n".
#[test]
fn split_line_lf() {
    let mut buf = make_buffer(b"alpha\nbeta\ngamma\n".to_vec());
    // Line 1 is "beta\n" starting at byte 6.
    buf.split_line(1, 2).unwrap();
    // Branch is now "alpha\nbe\nta\ngamma\n"
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(result, b"alpha\nbe\nta\ngamma\n".as_ref());
    // Line count increased by 1.
    assert_eq!(buf.line_offset(2).unwrap(), 9, "ta starts at 9");
    assert_eq!(buf.line_offset(3).unwrap(), 12, "gamma at 12");
}

/// Split "beta\r\n" at column 2 (after "be") → "be\r\nta\r\n".
#[test]
fn split_line_crlf() {
    let mut buf = make_buffer(b"alpha\r\nbeta\r\ngamma\r\n".to_vec());
    // Line 1 is "beta\r\n" starting at byte 7.
    buf.split_line(1, 2).unwrap();
    // Branch: "alpha\r\nbe\r\nta\r\ngamma\r\n"
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(result, b"alpha\r\nbe\r\nta\r\ngamma\r\n".as_ref());
}

// ── append_line ───────────────────────────────────────────────────────────────

/// append_line on an LF file appends a bare "\n".
#[test]
fn append_line_lf() {
    let mut buf = make_buffer(b"hello\nworld\n".to_vec());
    buf.append_line().unwrap();
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(result, b"hello\nworld\n\n".as_ref());
}

/// append_line on a CRLF file appends "\r\n".
#[test]
fn append_line_crlf() {
    let mut buf = make_buffer(b"hello\r\nworld\r\n".to_vec());
    buf.append_line().unwrap();
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(result, b"hello\r\nworld\r\n\r\n".as_ref());
}

// ── insert_line + line_offset consistency ─────────────────────────────────────

/// Inserting at line 0 shifts all lines down and leaves a blank line 0.
#[test]
fn insert_line_at_zero_shifts_all() {
    let mut buf = make_buffer(b"one\ntwo\nthree\n".to_vec());
    buf.insert_line(0).unwrap();
    // Branch: "\none\ntwo\nthree\n" (15 bytes)
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(result, b"\none\ntwo\nthree\n".as_ref());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert_eq!(buf.line_offset(1).unwrap(), 1, "one starts at 1");
    assert_eq!(buf.line_offset(2).unwrap(), 5, "two starts at 5");
}

/// Two consecutive inserts accumulate correctly.
#[test]
fn two_consecutive_insert_lines() {
    let mut buf = make_buffer(b"A\nB\nC\n".to_vec());
    buf.insert_line(1).unwrap(); // "\n" before B → "A\n\nB\nC\n"
    buf.insert_line(2).unwrap(); // "\n" before B (now at line 2) → "A\n\n\nB\nC\n"
    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(result, b"A\n\n\nB\nC\n".as_ref());
}

// ── apply_edit idempotency ────────────────────────────────────────────────────

/// Applying two sequential replacements on the same Buffer keeps state
/// consistent: first replaces "bar\n" → "BAR\n", then replaces "BAR\n" → "bar\n"
/// (round-trip); final result equals the original.
#[test]
fn apply_edit_idempotent_lf() {
    let source = b"foo\nbar\nbaz\n";
    let mut buf = make_buffer(source.to_vec());

    // First replacement: "bar\n" → "BAR\n"
    {
        let total = buf.branch().byte_len();
        let view = buf.view_range(0..total).unwrap();
        let pattern = b"bar\n";
        let pos = view
            .bytes
            .windows(pattern.len())
            .position(|w| w == pattern)
            .expect("bar present") as u64;
        buf.apply_edit(&view, pos..pos + pattern.len() as u64, b"BAR\n")
            .unwrap();
    }
    let after_first = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(after_first, b"foo\nBAR\nbaz\n".as_ref());

    // Second replacement: "BAR\n" → "bar\n" (round-trip).
    {
        let total = buf.branch().byte_len();
        let view = buf.view_range(0..total).unwrap();
        let pattern = b"BAR\n";
        let pos = view
            .bytes
            .windows(pattern.len())
            .position(|w| w == pattern)
            .expect("BAR present after first edit") as u64;
        buf.apply_edit(&view, pos..pos + pattern.len() as u64, b"bar\n")
            .unwrap();
    }
    let after_second = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(
        after_second,
        source.as_ref(),
        "round-trip returns to original"
    );
}

// ── terminator fidelity: insert + apply sequence on CRLF ─────────────────────

/// Full sequence: insert, delete, then regex replace on a CRLF file, checking
/// that CRLF is preserved end-to-end.
#[test]
fn full_sequence_crlf_terminator_fidelity() {
    // Source: 4 CRLF lines.
    let source = b"line0\r\nline1\r\nline2\r\nline3\r\n";
    let mut buf = make_buffer(source.to_vec());

    // Insert before line 1 → "line0\r\n\r\nline1\r\nline2\r\nline3\r\n"
    buf.insert_line(1).unwrap();
    assert_eq!(buf.line_offset(1).unwrap(), 7, "blank at 7");
    assert_eq!(buf.line_offset(2).unwrap(), 9, "line1 at 9");

    // Delete the blank line (line 1).
    {
        let v = buf.line_content(1).unwrap();
        let n = v.bytes.len() as u64;
        buf.apply_edit(&v, 0..n, &[]).unwrap();
    }

    // Verify restoration.
    let restored = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(restored, source.as_ref(), "restored to original");

    // Replace "line1\r\nline2\r\n" → wait, view is normalised: "line1\nline2\n"
    // apply_edit with raw CRLF replacement:
    let total = buf.branch().byte_len();
    let view = buf.view_range(0..total).unwrap();
    let pat = b"line1\nline2\n";
    let pos = view
        .bytes
        .windows(pat.len())
        .position(|w| w == pat)
        .expect("pattern found") as u64;
    buf.apply_edit(&view, pos..pos + pat.len() as u64, b"LINE1\r\nLINE2\r\n")
        .unwrap();

    let result = materialize(buf.branch().as_ref()).unwrap();
    assert_eq!(
        result,
        b"line0\r\nLINE1\r\nLINE2\r\nline3\r\n".as_ref(),
        "CRLF fidelity preserved after full sequence"
    );
}
