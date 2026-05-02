// Copyright (c) 2026, Michael Grier

//! Unit tests for `DenormaliseWriter` (MA-32).
//!
//! Every test writes normalised (LF-only) bytes through the writer and
//! compares the output against expected re-terminated bytes.

use std::io::Write;

use crate::{denormalise::DenormaliseWriter, encoding::LineEnding};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Write `input` through a `DenormaliseWriter` backed by `terminators`, call
/// `finish`, and return the accumulated output bytes.
fn run(input: &[u8], terminators: impl Iterator<Item = LineEnding>) -> Vec<u8> {
    let mut dw = DenormaliseWriter::new(Vec::<u8>::new(), terminators);
    dw.write_all(input).unwrap();
    dw.finish().unwrap()
}

/// Shorthand: build a terminator iterator from a slice literal.
fn terms(les: &[LineEnding]) -> impl Iterator<Item = LineEnding> + '_ {
    les.iter().copied()
}

// ── M == N cases ──────────────────────────────────────────────────────────────

/// M == N, all-LF: every \n stays \n.
#[test]
fn m_eq_n_all_lf() {
    let input = b"line1\nline2\nline3\n";
    let out = run(
        input,
        terms(&[LineEnding::Lf, LineEnding::Lf, LineEnding::Lf]),
    );
    assert_eq!(out, b"line1\nline2\nline3\n");
}

/// M == N, all-CRLF: every \n is replaced by \r\n.
#[test]
fn m_eq_n_all_crlf() {
    let input = b"line1\nline2\nline3\n";
    let out = run(
        input,
        terms(&[LineEnding::CrLf, LineEnding::CrLf, LineEnding::CrLf]),
    );
    assert_eq!(out, b"line1\r\nline2\r\nline3\r\n");
}

/// M == N, mixed terminators: each \n gets its own original terminator.
#[test]
fn m_eq_n_mixed() {
    // original had LF, CRLF, CR in that order
    let input = b"a\nb\nc\n";
    let out = run(
        input,
        terms(&[LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr]),
    );
    assert_eq!(out, b"a\nb\r\nc\r");
}

// ── M < N cases (replacement has more \n than original) ──────────────────────

/// M < N: first M newlines get original terminators; the rest are plain \n.
#[test]
fn m_lt_n_extra_newlines_become_lf() {
    // original had 1 terminator (CRLF); replacement has 3 \n
    let input = b"line1\nline2\nline3\n";
    let out = run(input, terms(&[LineEnding::CrLf]));
    assert_eq!(out, b"line1\r\nline2\nline3\n");
}

/// M < N with zero original terminators: all newlines pass through verbatim.
#[test]
fn m_lt_n_zero_terminators() {
    let input = b"a\nb\nc\n";
    let out = run(input, terms(&[]));
    assert_eq!(out, b"a\nb\nc\n");
}

// ── M > N cases (replacement has fewer \n than original) ─────────────────────

/// M > N: all N replacement newlines are substituted; surplus terminators are
/// emitted by `finish`.
#[test]
fn m_gt_n_surplus_emitted_by_finish() {
    // original had 3 terminators (CRLF, CRLF, CRLF); replacement has 1 \n
    let input = b"only\n";
    let out = run(
        input,
        terms(&[LineEnding::CrLf, LineEnding::CrLf, LineEnding::CrLf]),
    );
    assert_eq!(out, b"only\r\n\r\n\r\n");
}

/// M > N with mixed surplus terminators.
#[test]
fn m_gt_n_mixed_surplus() {
    // original: LF, CRLF, CR; replacement: 1 \n
    let input = b"x\n";
    let out = run(
        input,
        terms(&[LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr]),
    );
    assert_eq!(out, b"x\n\r\n\r");
}

// ── zero-line replacement ─────────────────────────────────────────────────────

/// Zero newlines in replacement, zero terminators: empty output is a no-op.
#[test]
fn zero_line_replacement_empty() {
    let out = run(b"", terms(&[]));
    assert_eq!(out, b"");
}

/// Zero newlines in replacement, surplus terminators: finish emits all of them.
#[test]
fn zero_line_replacement_with_surplus() {
    let out = run(b"", terms(&[LineEnding::CrLf, LineEnding::CrLf]));
    assert_eq!(out, b"\r\n\r\n");
}

// ── replacement with no trailing newline ──────────────────────────────────────

/// Replacement text has no trailing newline (final line without terminator).
#[test]
fn replacement_no_trailing_newline() {
    let input = b"no newline at end";
    let out = run(input, terms(&[]));
    assert_eq!(out, b"no newline at end");
}

/// Replacement with one internal newline but no trailing newline, M == N.
#[test]
fn replacement_internal_newline_no_trailing() {
    let input = b"first\nsecond";
    let out = run(input, terms(&[LineEnding::CrLf]));
    assert_eq!(out, b"first\r\nsecond");
}

// ── purely whitespace replacement ─────────────────────────────────────────────

/// Replacement that is purely whitespace (spaces and tabs) — no newlines.
#[test]
fn replacement_purely_whitespace_no_newlines() {
    let input = b"   \t   ";
    let out = run(input, terms(&[]));
    assert_eq!(out, b"   \t   ");
}

/// Whitespace-only replacement with surplus terminators (M > N, N == 0).
#[test]
fn replacement_whitespace_with_surplus() {
    let input = b"   ";
    let out = run(input, terms(&[LineEnding::CrLf]));
    assert_eq!(out, b"   \r\n");
}

// ── all-CRLF iterator with LF replacement text ───────────────────────────────

/// CRLF iterator supplied but replacement already uses \n: output is all \r\n.
#[test]
fn crlf_iterator_with_lf_replacement() {
    // Replacement is three LF-terminated lines; iterator supplies CRLFs.
    let input = b"alpha\nbeta\ngamma\n";
    let out = run(
        input,
        terms(&[LineEnding::CrLf, LineEnding::CrLf, LineEnding::CrLf]),
    );
    assert_eq!(out, b"alpha\r\nbeta\r\ngamma\r\n");
}

// ── empty input ───────────────────────────────────────────────────────────────

/// Empty input with empty iterator: nothing written.
#[test]
fn empty_input_empty_iterator() {
    let out = run(b"", terms(&[]));
    assert_eq!(out, b"");
}

/// Empty input with non-empty iterator: finish emits all terminators.
#[test]
fn empty_input_nonempty_iterator() {
    let out = run(b"", terms(&[LineEnding::Lf, LineEnding::CrLf]));
    assert_eq!(out, b"\n\r\n");
}

// ── multi-chunk writes ────────────────────────────────────────────────────────

/// Writing in multiple small chunks produces the same result as a single write.
#[test]
fn multi_chunk_write_same_as_single() {
    let terminators_single = [LineEnding::CrLf, LineEnding::Lf, LineEnding::Cr];
    let terminators_chunk = [LineEnding::CrLf, LineEnding::Lf, LineEnding::Cr];

    // single write
    let single = run(b"a\nb\nc\n", terms(&terminators_single));

    // byte-by-byte writes
    let mut dw = DenormaliseWriter::new(Vec::<u8>::new(), terms(&terminators_chunk));
    for &byte in b"a\nb\nc\n" {
        dw.write_all(&[byte]).unwrap();
    }
    let chunk = dw.finish().unwrap();

    assert_eq!(single, chunk);
}

// ── CR-only original terminators ─────────────────────────────────────────────

/// All-CR original terminators: each \n in the replacement becomes \r.
#[test]
fn all_cr_terminators() {
    let input = b"x\ny\n";
    let out = run(input, terms(&[LineEnding::Cr, LineEnding::Cr]));
    assert_eq!(out, b"x\ry\r");
}

// ── into_inner skips surplus terminators ─────────────────────────────────────

/// `into_inner` does not emit surplus terminators even when M > N.
#[test]
fn into_inner_skips_surplus() {
    let mut dw = DenormaliseWriter::new(
        Vec::<u8>::new(),
        terms(&[LineEnding::CrLf, LineEnding::CrLf]),
    );
    dw.write_all(b"").unwrap(); // 0 newlines written, 2 surplus
    let inner = dw.into_inner();
    assert_eq!(inner, b""); // surplus NOT emitted
}
