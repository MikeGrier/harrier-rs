// Copyright (c) 2026, Michael Grier

//! MA-IT-2: Milestone 2 integration test — sed-mode replace.
//!
//! Opens a CRLF file and an LF file; for each:
//!   1. Iterates all lines via `Lines` to build a `TerminatorLog`.
//!   2. Materialises the full file as a `View` via `view_range`.
//!   3. Finds a pattern in the normalised view bytes.
//!   4. Denormalises the replacement text by piping it through
//!      `DenormaliseWriter` with the terminators covering the matched region
//!      from the log.
//!   5. Splices the denormalised replacement into the source via `View::apply`.
//!   6. Verifies the output preserves original per-line terminators.
//!   7. Runs the same operation a second time to verify idempotency and
//!      original-coordinate stability.

use std::{io::Write, sync::Arc};

use encoding_rs::UTF_8;
use weaver::{
    denormalise::DenormaliseWriter,
    encoding::{BomPolicy, LineEnding, SourceConfig},
    lines::{LineTerminator, Lines, TerminatorLog},
    source::Source,
};
use redwing::{make_thicket_from_bytes, materialize, Branch};

// ── helpers ───────────────────────────────────────────────────────────────────

fn branch_of(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

fn make_lines(bytes: impl Into<Vec<u8>>) -> Lines {
    let b = branch_of(bytes);
    Source::new(
        b,
        SourceConfig {
            encoding_hint: Some(UTF_8),
            bom_policy: BomPolicy::Ignore,
            ..SourceConfig::default()
        },
    )
    .unwrap()
    .as_lines()
    .unwrap()
}

/// Perform one sed-mode replace pass:
///
/// 1. Iterate `lines` to build a full `TerminatorLog`.
/// 2. `view_range` the full branch.
/// 3. Search for `pattern` in the normalised view bytes.
///    Returns `None` if the pattern is not found — output bytes equal input.
/// 4. Denormalise `replacement` (which must be LF-only) using the terminators
///    that cover the matched region.
/// 5. `apply` the denormalised replacement and materialise the result.
fn sed_replace(mut lines: Lines, branch_len: u64, pattern: &[u8], replacement: &[u8]) -> Vec<u8> {
    // ── Step 1: collect terminator log ────────────────────────────────────────
    let mut log = TerminatorLog::new(4096);
    for (_, term) in &mut lines {
        if let LineTerminator::Ending(le) = term {
            log.push(le);
        }
    }

    // ── Step 2: materialise normalised view ───────────────────────────────────
    let view = lines.view_range(0..branch_len).expect("view_range");
    let norm = &view.bytes;

    // ── Step 3: find pattern ──────────────────────────────────────────────────
    let match_start = match norm.windows(pattern.len()).position(|w| w == pattern) {
        Some(pos) => pos as u64,
        // Pattern not found: return the original source bytes unchanged.
        None => return materialize(lines.branch().as_ref()).unwrap(),
    };
    let match_end = match_start + pattern.len() as u64;

    // ── Step 4: denormalise the replacement ───────────────────────────────────
    // Count how many newlines occur in the normalised bytes *before* the match
    // so we know which terminator slot to start from.
    let pre_newlines = norm[..match_start as usize]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();

    // Count newlines *inside* the matched region to know how many terminators
    // to consume.
    let match_newlines = norm[match_start as usize..match_end as usize]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();

    let denorm_replacement = {
        let terms = log.iter().skip(pre_newlines).take(match_newlines);
        let mut dw = DenormaliseWriter::new(Vec::<u8>::new(), terms);
        dw.write_all(replacement).unwrap();
        dw.finish().unwrap()
    };

    // ── Step 5: splice and materialise ───────────────────────────────────────
    let result_branch = view
        .apply(match_start..match_end, &denorm_replacement)
        .expect("apply");
    materialize(result_branch.as_ref()).unwrap()
}

// ── tests ──────────────────────────────────────────────────────────────────────

// ── Single-line replacement: CRLF source ─────────────────────────────────────

/// CRLF source, single-line match — replacement re-acquires CRLF from log.
#[test]
fn single_line_replace_crlf() {
    let source = b"foo\r\nbar baz\r\nqux\r\n";
    // Normalised: "foo\nbar baz\nqux\n"
    // Pattern:    "bar baz\n"  at normalised 4..12
    // Replacement (normalised): "REPLACED\n"
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"bar baz\n", b"REPLACED\n");
    assert_eq!(result, b"foo\r\nREPLACED\r\nqux\r\n");
}

// ── Single-line replacement: LF source ───────────────────────────────────────

/// LF source, single-line match — replacement stays LF.
#[test]
fn single_line_replace_lf() {
    let source = b"foo\nbar baz\nqux\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"bar baz\n", b"REPLACED\n");
    assert_eq!(result, b"foo\nREPLACED\nqux\n");
}

// ── Single-line replacement: CR source ───────────────────────────────────────

/// CR-only source, single-line match — replacement re-acquires CR from log.
#[test]
fn single_line_replace_cr() {
    let source = b"foo\rbar baz\rqux\r";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"bar baz\n", b"REPLACED\n");
    assert_eq!(result, b"foo\rREPLACED\rqux\r");
}

// ── Multi-line replacement: CRLF source ──────────────────────────────────────

/// CRLF source, two-line pattern — both replacement lines re-acquire CRLF.
#[test]
fn multi_line_replace_crlf() {
    // Source: "alpha\r\nbeta\r\ngamma\r\n" (20 bytes)
    // Normalised: "alpha\nbeta\ngamma\n"
    // Pattern:    "beta\ngamma\n"  (11 bytes) at normalised pos 6..17
    let source = b"alpha\r\nbeta\r\ngamma\r\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"beta\ngamma\n", b"BETA\nGAMMA\n");
    assert_eq!(result, b"alpha\r\nBETA\r\nGAMMA\r\n");
}

/// CRLF source, two-line pattern — replacement has more lines than original.
#[test]
fn multi_line_replace_crlf_expansion() {
    // "alpha\r\nBETA\r\n" → replace "alpha\n" with "X\nY\nZ\n" (3 lines)
    let source = b"alpha\r\nbeta\r\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    // Replacement has 3 newlines but log only has 2 terminators for "alpha" line.
    // DenormaliseWriter uses what terminators it has then falls back to plain \n.
    let result = sed_replace(lines, len, b"alpha\n", b"X\nY\n");
    // "alpha\n" replaced by "X\r\nY\r\n" (consumes the CrLf terminator once)
    // DenormaliseWriter with 1 terminator: X→CrLf, Y→exhausted→\n kept as-is? No:
    // DenormaliseWriter::write pops from iterator; when exhausted, writes \n verbatim.
    // replacement "X\nY\n": first \n → CrLf, second \n → iterator exhausted → \n verbatim
    assert_eq!(result, b"X\r\nY\nbeta\r\n");
}

/// LF source, two-line pattern — replacement stays LF.
#[test]
fn multi_line_replace_lf() {
    let source = b"alpha\nbeta\ngamma\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"beta\ngamma\n", b"BETA\nGAMMA\n");
    assert_eq!(result, b"alpha\nBETA\nGAMMA\n");
}

// ── Pattern at start of file ──────────────────────────────────────────────────

/// CRLF source, pattern at the very start of the file.
#[test]
fn pattern_at_start_crlf() {
    let source = b"hello\r\nworld\r\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"hello\n", b"HI\n");
    assert_eq!(result, b"HI\r\nworld\r\n");
}

/// LF source, pattern at the very start.
#[test]
fn pattern_at_start_lf() {
    let source = b"hello\nworld\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"hello\n", b"HI\n");
    assert_eq!(result, b"HI\nworld\n");
}

// ── Pattern at end of file ────────────────────────────────────────────────────

/// CRLF source, pattern at the very end.
#[test]
fn pattern_at_end_crlf() {
    let source = b"hello\r\nworld\r\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"world\n", b"EARTH\n");
    assert_eq!(result, b"hello\r\nEARTH\r\n");
}

/// LF source, pattern at the very end.
#[test]
fn pattern_at_end_lf() {
    let source = b"hello\nworld\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"world\n", b"EARTH\n");
    assert_eq!(result, b"hello\nEARTH\n");
}

// ── Idempotency ───────────────────────────────────────────────────────────────

/// CRLF: running the same replacement twice gives the same output.
///
/// After the first pass, "bar baz" no longer exists in the output, so the
/// second pass finds no match and returns the bytes unchanged.
#[test]
fn idempotency_crlf() {
    let source = b"foo\r\nbar baz\r\nqux\r\n";

    // Round 1
    let round1 = {
        let lines = make_lines(source.as_ref());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"bar baz\n", b"REPLACED\n")
    };
    assert_eq!(round1, b"foo\r\nREPLACED\r\nqux\r\n");

    // Round 2 — pattern no longer present; output unchanged.
    let round2 = {
        let lines = make_lines(round1.clone());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"bar baz\n", b"REPLACED\n")
    };
    assert_eq!(round1, round2, "second pass must be idempotent");
}

/// LF: idempotency.
#[test]
fn idempotency_lf() {
    let source = b"foo\nbar baz\nqux\n";

    let round1 = {
        let lines = make_lines(source.as_ref());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"bar baz\n", b"REPLACED\n")
    };
    assert_eq!(round1, b"foo\nREPLACED\nqux\n");

    let round2 = {
        let lines = make_lines(round1.clone());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"bar baz\n", b"REPLACED\n")
    };
    assert_eq!(round1, round2);
}

/// Multi-line CRLF: idempotency.
#[test]
fn idempotency_multi_line_crlf() {
    let source = b"alpha\r\nbeta\r\ngamma\r\n";

    let round1 = {
        let lines = make_lines(source.as_ref());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"beta\ngamma\n", b"BETA\nGAMMA\n")
    };
    assert_eq!(round1, b"alpha\r\nBETA\r\nGAMMA\r\n");

    let round2 = {
        let lines = make_lines(round1.clone());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"beta\ngamma\n", b"BETA\nGAMMA\n")
    };
    assert_eq!(round1, round2);
}

// ── Original-coordinate stability ────────────────────────────────────────────

/// After a CRLF replacement, opening the result as `Lines` and calling
/// `view_range` gives normalised bytes consistent with the new content.
/// This verifies that coordinate translation in the result branch is stable.
#[test]
fn coordinate_stability_after_replace_crlf() {
    let source = b"foo\r\nbar baz\r\nqux\r\n";

    // Round 1: replace "bar baz" line.
    let round1 = {
        let lines = make_lines(source.as_ref());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"bar baz\n", b"REPLACED\n")
    };
    // round1 = b"foo\r\nREPLACED\r\nqux\r\n"

    // Open round1 as Lines and view_range the whole file.
    let lines2 = make_lines(round1.clone());
    let len2 = lines2.branch().byte_len();
    let view2 = lines2.view_range(0..len2).unwrap();

    // Normalised bytes of the result must reflect the replacement.
    assert_eq!(view2.bytes, b"foo\nREPLACED\nqux\n");

    // Applying a replacement on this view at the correct normalised position
    // must splice into the source correctly.
    // "REPLACED\n" is at normalised 4..13.
    let fork = view2.apply(4..13, b"AGAIN\n").unwrap();
    let forked_bytes = materialize(fork.as_ref()).unwrap();
    // apply inserts raw bytes; the source still has CRLF for unchanged parts.
    assert_eq!(forked_bytes, b"foo\r\nAGAIN\nqux\r\n");
    // When the caller denormalises the replacement, the CRLF is restored:
    let fork2 = view2.apply(4..13, b"AGAIN\r\n").unwrap();
    let forked2 = materialize(fork2.as_ref()).unwrap();
    assert_eq!(forked2, b"foo\r\nAGAIN\r\nqux\r\n");
}

/// After an LF replacement, view_range on the result has stable coordinates.
#[test]
fn coordinate_stability_after_replace_lf() {
    let source = b"foo\nbar baz\nqux\n";

    let round1 = {
        let lines = make_lines(source.as_ref());
        let len = lines.branch().byte_len();
        sed_replace(lines, len, b"bar baz\n", b"REPLACED\n")
    };
    // round1 = b"foo\nREPLACED\nqux\n"

    let lines2 = make_lines(round1.clone());
    let len2 = lines2.branch().byte_len();
    let view2 = lines2.view_range(0..len2).unwrap();

    // For LF source, normalised == source bytes.
    assert_eq!(view2.bytes, round1.as_slice());

    // Splice at verified normalised position 4..13 = "REPLACED\n".
    let fork = view2.apply(4..13, b"AGAIN\n").unwrap();
    let result = materialize(fork.as_ref()).unwrap();
    assert_eq!(result, b"foo\nAGAIN\nqux\n");
}

// ── Pattern not found ─────────────────────────────────────────────────────────

/// When the pattern is absent, the original bytes are returned unchanged.
#[test]
fn no_match_returns_original_crlf() {
    let source = b"hello\r\nworld\r\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"notpresent\n", b"anything\n");
    assert_eq!(result, source.as_ref());
}

/// LF: pattern absent → original returned.
#[test]
fn no_match_returns_original_lf() {
    let source = b"hello\nworld\n";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"notpresent\n", b"anything\n");
    assert_eq!(result, source.as_ref());
}

// ── Terminator log fidelity ───────────────────────────────────────────────────

/// Mixed-terminator file: each replacement line re-acquires its original
/// per-line terminator from the log.
#[test]
fn mixed_terminator_replace() {
    // Line 1: LF, Line 2: CRLF, Line 3: CR
    // "a\nb\r\nc\r" — 3 lines
    // Replace "b\n" (normalised) with "BB\n"
    // Line 2's terminator is CrLf → substitute → "BB\r\n"
    let source = b"a\nb\r\nc\r";
    let lines = make_lines(source.as_ref());
    let len = lines.branch().byte_len();
    let result = sed_replace(lines, len, b"b\n", b"BB\n");
    assert_eq!(result, b"a\nBB\r\nc\r");
}

/// Verify terminator log records all three terminator kinds in source order.
#[test]
fn terminator_log_fidelity() {
    let source = b"x\ny\r\nz\r";
    let mut lines = make_lines(source.as_ref());
    let mut log = TerminatorLog::new(10);
    for (_, term) in &mut lines {
        if let LineTerminator::Ending(le) = term {
            log.push(le);
        }
    }
    let kinds: Vec<_> = log.iter().collect();
    assert_eq!(kinds, [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr]);
}
