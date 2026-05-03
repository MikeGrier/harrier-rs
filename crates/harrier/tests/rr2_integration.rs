// Copyright (c) 2026, Michael Grier

//! MA-IT-4: Integration tests for the rr2 example.
//!
//! Each test drives `rr2_run`, an in-memory replica of the core rr2 pipeline
//! (Source → Lines → view_range → regex → collect splices → fork+splice →
//! materialize) that avoids disk I/O so the tests are fast and self-contained.
//!
//! # Test organisation
//!
//! A: Edge / tiny files (0–2 bytes)
//! B: Basic LF functionality (10 tests carried from MA-IT-4 v1)
//! C: LF files — encoding marker at ambiguity distances 0, 10, 100, 1k, 10k, 100k
//! D: CRLF files — same ambiguity distances
//! E: Error-byte injection at distances 10, 100, 1k, 10k, 100k (LF source)
//! F: Error-byte injection (CRLF source), cross-distance
//! G: Error kind variants (invalid_seq / truncated_seq / overlong / surrogate)
//! H: Pattern placement relative to encoding marker
//! I: Large files — many TARGET occurrences
//! J: Regex-feature edge cases
//! K: Replacement edge cases
//! L: BOM stripping / detection
//! M: Lone-CR and mixed-terminator sources
//! N: CRLF source with error bytes, cross-line match near error
//!
//! # Failure messages
//!
//! All long-content assertions use `assert_rr2` / `assert_starts` helpers
//! that print a hex dump of the first 300 bytes of actual and expected on
//! failure so failures are debuggable without access to generated data.

use std::{io::Write as _, sync::Arc};

use encoding_rs::UTF_8;
use harrier::{
    denormalise::DenormaliseWriter,
    encoding::{BomPolicy, LineEnding, SourceConfig},
    source::Source,
};
use redwing::{make_thicket_from_bytes, materialize, Branch};
use regex::bytes::Regex;

// ═══════════════════════════════════════════════════════════════════════════
// Core pipeline helper
// ═══════════════════════════════════════════════════════════════════════════

/// In-memory replica of the rr2 core pipeline.
///
/// Accepts raw `content` bytes, applies the regex replacement, and returns
/// the transformed bytes.  `encoding_hint` can be `None` to let chardetng
/// auto-detect (useful for non-UTF-8 content tests).
fn rr2_run(
    content: &[u8],
    pattern: &str,
    replacement: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    rr2_run_cfg(content, pattern, replacement, Some(UTF_8))
}

fn rr2_run_cfg(
    content: &[u8],
    pattern: &str,
    replacement: &[u8],
    encoding_hint: Option<&'static encoding_rs::Encoding>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let b1: Arc<dyn Branch> = make_thicket_from_bytes(content.to_vec()).main();
    let file_len = b1.byte_len();

    let source = Source::new(
        Arc::clone(&b1),
        SourceConfig {
            encoding_hint,
            bom_policy: BomPolicy::Honour,
            ..SourceConfig::default()
        },
    )?;
    let line_ending = source.line_ending();

    let lines = source.as_lines()?;
    let view = lines.view_range(0..file_len)?;

    let re = Regex::new(pattern)?;

    struct Splice {
        source_start: u64,
        source_len: u64,
        content: Vec<u8>,
    }

    let mut splices: Vec<Splice> = re
        .captures_iter(&view.bytes)
        .map(|caps| {
            let m = caps.get(0).unwrap();
            let norm_start = m.start() as u64;
            let norm_end = m.end() as u64;
            let source_start = view.byte_range_start() + view.offset_map.to_source(norm_start);
            let source_end = view.byte_range_start() + view.offset_map.to_source(norm_end);
            let source_len = source_end - source_start;

            let mut norm_repl = Vec::new();
            caps.expand(replacement, &mut norm_repl);
            let content = denormalise_bytes(&norm_repl, line_ending);

            Splice {
                source_start,
                source_len,
                content,
            }
        })
        .collect();

    let b2 = b1.fork();
    splices.sort_unstable_by_key(|b| std::cmp::Reverse(b.source_start));
    for s in &splices {
        b2.splice(s.source_start, s.source_len, &s.content)?;
    }

    Ok(materialize(&*b2)?)
}

fn denormalise_bytes(norm: &[u8], le: LineEnding) -> Vec<u8> {
    let mut dw = DenormaliseWriter::new(Vec::with_capacity(norm.len()), std::iter::repeat(le));
    dw.write_all(norm).unwrap();
    dw.into_inner()
}

// ═══════════════════════════════════════════════════════════════════════════
// Content generator
// ═══════════════════════════════════════════════════════════════════════════

/// Generate synthetic file content for rr2 pipeline tests.
///
/// Lines alternate: every 10th line (starting at 0) is `"TARGET000000"`;
/// all other lines are `"fillerNNNNNN"`.  Each line is terminated by
/// `line_ending`.  The encoding marker is injected (as raw bytes, outside
/// any line) after `ambiguity_bytes` of content; the error bytes are injected
/// at `error_offset`.
///
/// The total content length will be at least `min_length` bytes.
fn generate(
    min_length: usize,
    line_ending: &[u8],
    ambiguity_bytes: usize,
    encoding_marker: &[u8],
    error_offset: Option<usize>,
    error_bytes: &[u8],
) -> Vec<u8> {
    if min_length == 0 {
        return Vec::new();
    }
    let mut out: Vec<u8> = Vec::with_capacity(min_length + 128);
    let mut line_num: usize = 0;
    let mut marker_done = encoding_marker.is_empty() || ambiguity_bytes == usize::MAX;
    let mut error_done = error_bytes.is_empty() || error_offset.is_none();

    while out.len() < min_length {
        let pos = out.len();

        if !marker_done && pos >= ambiguity_bytes {
            out.extend_from_slice(encoding_marker);
            marker_done = true;
            continue;
        }
        if !error_done
            && let Some(off) = error_offset
            && pos >= off
        {
            out.extend_from_slice(error_bytes);
            error_done = true;
            continue;
        }

        let body: &[u8] = if line_num.is_multiple_of(10) {
            b"TARGET"
        } else {
            b"filler"
        };
        out.extend_from_slice(body);
        let num = format!("{line_num:06}");
        out.extend_from_slice(num.as_bytes());
        out.extend_from_slice(line_ending);
        line_num += 1;
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// Assertion helpers
// ═══════════════════════════════════════════════════════════════════════════

fn hex_sample(buf: &[u8]) -> String {
    let n = buf.len().min(300);
    buf[..n]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[track_caller]
fn assert_rr2(actual: &[u8], expected: &[u8], context: &str) {
    if actual != expected {
        panic!(
            "rr2 output mismatch [{context}]\n\
             actual   ({} B): {}\n\
             expected ({} B): {}",
            actual.len(),
            hex_sample(actual),
            expected.len(),
            hex_sample(expected),
        );
    }
}

#[track_caller]
fn assert_starts(actual: &[u8], prefix: &[u8], context: &str) {
    if !actual.starts_with(prefix) {
        panic!(
            "rr2 prefix mismatch [{context}]\n\
             actual start ({} B total): {}\n\
             expected prefix ({} B):    {}",
            actual.len(),
            hex_sample(actual),
            prefix.len(),
            hex_sample(prefix),
        );
    }
}

#[track_caller]
fn assert_contains(actual: &[u8], needle: &[u8], context: &str) {
    if !actual.windows(needle.len()).any(|w| w == needle) {
        panic!(
            "rr2 content missing [{context}]\n\
             actual ({} B): {}\n\
             needle ({} B): {}",
            actual.len(),
            hex_sample(actual),
            needle.len(),
            hex_sample(needle),
        );
    }
}

#[track_caller]
fn assert_not_contains(actual: &[u8], needle: &[u8], context: &str) {
    if actual.windows(needle.len()).any(|w| w == needle) {
        panic!(
            "rr2 unexpectedly contains [{context}]\n\
             actual ({} B): {}\n\
             needle ({} B): {}",
            actual.len(),
            hex_sample(actual),
            needle.len(),
            hex_sample(needle),
        );
    }
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut pos = 0;
    while pos + needle.len() <= haystack.len() {
        if &haystack[pos..pos + needle.len()] == needle {
            count += 1;
            pos += needle.len();
        } else {
            pos += 1;
        }
    }
    count
}

// ═══════════════════════════════════════════════════════════════════════════
// A. Edge / tiny files
// ═══════════════════════════════════════════════════════════════════════════

/// A-1: Zero-length file produces zero-length output.
#[test]
fn a1_empty_file_no_crash() {
    let out = rr2_run(b"", "TARGET", b"X").unwrap();
    assert_rr2(&out, b"", "A-1 empty");
}

/// A-2: Single ASCII byte, no match.
#[test]
fn a2_single_byte_no_match() {
    let out = rr2_run(b"a", "z", b"Z").unwrap();
    assert_rr2(&out, b"a", "A-2 single byte no match");
}

/// A-3: Single ASCII byte, exact match.
#[test]
fn a3_single_byte_match() {
    let out = rr2_run(b"x", "x", b"Y").unwrap();
    assert_rr2(&out, b"Y", "A-3 single byte match");
}

/// A-4: Single LF byte.  Pattern `\n` matches the normalised LF.
#[test]
fn a4_single_lf() {
    let out = rr2_run(b"\n", r"\n", b"NL").unwrap();
    assert_rr2(&out, b"NL", "A-4 single LF");
}

/// A-5: Single CRLF — normalises to one `\n` in the view; pattern `\n` hits it.
#[test]
fn a5_single_crlf_matches_in_norm_view() {
    let out = rr2_run(b"\r\n", r"\n", b".").unwrap();
    assert_rr2(&out, b".", "A-5 single CRLF norm match");
}

/// A-6: Two-byte file `"ab"`, match first byte.
#[test]
fn a6_two_bytes_match_first() {
    let out = rr2_run(b"ab", "a", b"A").unwrap();
    assert_rr2(&out, b"Ab", "A-6 two-byte match first");
}

/// A-7: Two-byte file `"ab"`, match second byte.
#[test]
fn a7_two_bytes_match_second() {
    let out = rr2_run(b"ab", "b", b"B").unwrap();
    assert_rr2(&out, b"aB", "A-7 two-byte match second");
}

/// A-8: File consisting solely of repeated newlines (LF).
#[test]
fn a8_all_newlines_lf() {
    let input = b"\n\n\n\n\n";
    let out = rr2_run(input, "TARGET", b"X").unwrap();
    assert_rr2(&out, input, "A-8 all newlines LF no match");
}

// ═══════════════════════════════════════════════════════════════════════════
// B. Basic functionality (carried from v1)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn b1_lf_simple_word_replace() {
    let out = rr2_run(b"foo\nbar\nbaz\n", "foo", b"FOO").unwrap();
    assert_rr2(&out, b"FOO\nbar\nbaz\n", "B-1");
}

#[test]
fn b2_crlf_preserves_terminators() {
    let out = rr2_run(b"foo\r\nbar\r\nbaz\r\n", "bar", b"BAR").unwrap();
    assert_rr2(&out, b"foo\r\nBAR\r\nbaz\r\n", "B-2");
}

#[test]
fn b3_capture_group_word_swap() {
    let out = rr2_run(b"hello world\ngoodbye moon\n", r"(\w+) (\w+)", b"$2 $1").unwrap();
    assert_rr2(&out, b"world hello\nmoon goodbye\n", "B-3");
}

#[test]
fn b4_no_matches_output_identical() {
    let input = b"foo\r\nbar\r\nbaz\r\n";
    let out = rr2_run(input, "zzz", b"anything").unwrap();
    assert_rr2(&out, input, "B-4");
}

#[test]
fn b5_multiple_occurrences_same_line() {
    let out = rr2_run(b"aaa bbb aaa ccc aaa\n", "aaa", b"xxx").unwrap();
    assert_rr2(&out, b"xxx bbb xxx ccc xxx\n", "B-5");
}

#[test]
fn b6_cross_line_replacement_lf() {
    let out = rr2_run(b"start\nfoo\nbar\nend\n", r"foo\nbar", b"baz").unwrap();
    assert_rr2(&out, b"start\nbaz\nend\n", "B-6");
}

#[test]
fn b7_no_trailing_newline() {
    let out = rr2_run(b"hello world", "world", b"earth").unwrap();
    assert_rr2(&out, b"hello earth", "B-7");
}

#[test]
fn b8_case_insensitive_flag() {
    let out = rr2_run(b"Hello\nhELLO\nhello\n", r"(?i)hello", b"Hi").unwrap();
    assert_rr2(&out, b"Hi\nHi\nHi\n", "B-8");
}

#[test]
fn b9_replacement_introduces_newlines_lf() {
    let out = rr2_run(b"foo\nbar\n", "foo", b"line1\nline2").unwrap();
    assert_rr2(&out, b"line1\nline2\nbar\n", "B-9");
}

#[test]
fn b10_replacement_introduces_newlines_crlf() {
    let out = rr2_run(b"foo\r\nbar\r\n", "foo", b"line1\nline2").unwrap();
    assert_rr2(&out, b"line1\r\nline2\r\nbar\r\n", "B-10");
}

// ═══════════════════════════════════════════════════════════════════════════
// C. LF source — encoding marker at increasing ambiguity distances
//
// For each test the generated content has `TARGET000000` at the very start
// (first line).  The pipeline replaces it with `PATCHED000000`.  The encoding
// marker bytes embedded after the ambiguity boundary must not corrupt the
// match or the untouched remainder.
// ═══════════════════════════════════════════════════════════════════════════

fn lf_latin1_marker_test(ambiguity_bytes: usize, min_length: usize, label: &str) {
    let content = generate(min_length, b"\n", ambiguity_bytes, b"\xa9", None, b"");
    let out = rr2_run(&content, "TARGET000000", b"PATCHED000000").unwrap();
    // Verify replacement applied and no original match words remain.
    assert_not_contains(&out, b"TARGET000000", label);
    assert_contains(&out, b"PATCHED000000", label);
    // Marker byte must be preserved in output (not consumed by the regex).
    assert_contains(&out, b"\xa9", label);
}

/// C-1: Latin-1 © (0xA9) injected at byte 0 — disambiguates immediately.
#[test]
fn c1_lf_marker_ambiguity_0() {
    lf_latin1_marker_test(0, 200, "C-1 LF ambiguity=0");
}

/// C-2: 10 bytes of ASCII before the Latin-1 marker.
#[test]
fn c2_lf_marker_ambiguity_10() {
    lf_latin1_marker_test(10, 300, "C-2 LF ambiguity=10");
}

/// C-3: 100 bytes of ASCII before the Latin-1 marker.
#[test]
fn c3_lf_marker_ambiguity_100() {
    lf_latin1_marker_test(100, 500, "C-3 LF ambiguity=100");
}

/// C-4: 1 000 bytes of ASCII before the Latin-1 marker.
#[test]
fn c4_lf_marker_ambiguity_1000() {
    lf_latin1_marker_test(1_000, 2_000, "C-4 LF ambiguity=1000");
}

/// C-5: 10 000 bytes of ASCII before the Latin-1 marker.
#[test]
fn c5_lf_marker_ambiguity_10000() {
    lf_latin1_marker_test(10_000, 12_000, "C-5 LF ambiguity=10000");
}

/// C-6: 100 000 bytes of ASCII before the Latin-1 marker.
/// chardetng's probe window is smaller than the ambiguity distance, so the
/// marker is invisible to the encoding detector; the pipeline uses UTF-8 and
/// treats 0xA9 as a raw byte in the view.
#[test]
fn c6_lf_marker_ambiguity_100000() {
    lf_latin1_marker_test(100_000, 102_000, "C-6 LF ambiguity=100000");
}

// ═══════════════════════════════════════════════════════════════════════════
// D. CRLF source — encoding marker at increasing ambiguity distances
// ═══════════════════════════════════════════════════════════════════════════

fn crlf_latin1_marker_test(ambiguity_bytes: usize, min_length: usize, label: &str) {
    let content = generate(min_length, b"\r\n", ambiguity_bytes, b"\xa9", None, b"");
    let out = rr2_run(&content, "TARGET000000", b"PATCHED000000").unwrap();
    // Verify replacement applied and no original match words remain.
    assert_not_contains(&out, b"TARGET000000", label);
    assert_contains(&out, b"PATCHED000000", label);
    assert_contains(&out, b"\xa9", label);
    // No bare LF should appear (DenormaliseWriter keeps CRLF).
    let lf_count = out.iter().filter(|&&b| b == b'\n').count();
    let crlf_count = out.windows(2).filter(|w| *w == b"\r\n").count();
    assert_eq!(
        lf_count, crlf_count,
        "[{label}] bare LF found in CRLF output (lf={lf_count} crlf={crlf_count})"
    );
}

/// D-1 through D-6: same ambiguity distances as C series but CRLF source.
#[test]
fn d1_crlf_marker_ambiguity_0() {
    crlf_latin1_marker_test(0, 200, "D-1 CRLF ambiguity=0");
}
#[test]
fn d2_crlf_marker_ambiguity_10() {
    crlf_latin1_marker_test(10, 300, "D-2 CRLF ambiguity=10");
}
#[test]
fn d3_crlf_marker_ambiguity_100() {
    crlf_latin1_marker_test(100, 500, "D-3 CRLF ambiguity=100");
}
#[test]
fn d4_crlf_marker_ambiguity_1000() {
    crlf_latin1_marker_test(1_000, 2_000, "D-4 CRLF ambiguity=1000");
}
#[test]
fn d5_crlf_marker_ambiguity_10000() {
    crlf_latin1_marker_test(10_000, 12_000, "D-5 CRLF ambiguity=10000");
}
#[test]
fn d6_crlf_marker_ambiguity_100000() {
    crlf_latin1_marker_test(100_000, 102_000, "D-6 CRLF ambiguity=100000");
}

// ═══════════════════════════════════════════════════════════════════════════
// E. Error-byte injection — LF source
//
// Invalid UTF-8 bytes injected at offset N.  chardetng may switch to a
// different detected encoding, but view_range operates on raw bytes, so the
// regex replacement still works correctly.  The first TARGET (line 0) is
// always before the error injection point.
// ═══════════════════════════════════════════════════════════════════════════

fn lf_error_test(error_offset: usize, min_length: usize, error_bytes: &[u8], label: &str) {
    let content = generate(
        min_length,
        b"\n",
        usize::MAX,
        b"",
        Some(error_offset),
        error_bytes,
    );
    let out = rr2_run_cfg(&content, "TARGET000000", b"PATCHED000000", None).unwrap();
    assert_starts(&out, b"PATCHED000000\n", label);
    assert_not_contains(&out, b"TARGET000000", label);
}

/// E-1: 0xFF 0xFF injected at byte 10.
#[test]
fn e1_lf_invalid_seq_at_10() {
    lf_error_test(10, 500, b"\xff\xff", "E-1 error at 10");
}

/// E-2: 0xFF 0xFF injected at byte 100.
#[test]
fn e2_lf_invalid_seq_at_100() {
    lf_error_test(100, 500, b"\xff\xff", "E-2 error at 100");
}

/// E-3: 0xFF 0xFF injected at byte 1 000.
#[test]
fn e3_lf_invalid_seq_at_1000() {
    lf_error_test(1_000, 2_000, b"\xff\xff", "E-3 error at 1000");
}

/// E-4: 0xFF 0xFF injected at byte 10 000.
#[test]
fn e4_lf_invalid_seq_at_10000() {
    lf_error_test(10_000, 12_000, b"\xff\xff", "E-4 error at 10000");
}

/// E-5: 0xFF 0xFF injected at byte 100 000.
#[test]
fn e5_lf_invalid_seq_at_100000() {
    lf_error_test(100_000, 102_000, b"\xff\xff", "E-5 error at 100000");
}

// ═══════════════════════════════════════════════════════════════════════════
// F. Error-byte injection — CRLF source
// ═══════════════════════════════════════════════════════════════════════════

fn crlf_error_test(error_offset: usize, min_length: usize, error_bytes: &[u8], label: &str) {
    let content = generate(
        min_length,
        b"\r\n",
        usize::MAX,
        b"",
        Some(error_offset),
        error_bytes,
    );
    let out = rr2_run_cfg(&content, "TARGET000000", b"PATCHED000000", None).unwrap();
    assert_starts(&out, b"PATCHED000000\r\n", label);
    assert_not_contains(&out, b"TARGET000000", label);
}

/// F-1 through F-4: CRLF source with 0xFF 0xFF injected.
#[test]
fn f1_crlf_error_at_10() {
    crlf_error_test(10, 500, b"\xff\xff", "F-1 CRLF error at 10");
}
#[test]
fn f2_crlf_error_at_100() {
    crlf_error_test(100, 500, b"\xff\xff", "F-2 CRLF error at 100");
}
#[test]
fn f3_crlf_error_at_1000() {
    crlf_error_test(1_000, 2_000, b"\xff\xff", "F-3 CRLF error at 1000");
}
#[test]
fn f4_crlf_error_at_10000() {
    crlf_error_test(10_000, 12_000, b"\xff\xff", "F-4 CRLF error at 10000");
}

// ═══════════════════════════════════════════════════════════════════════════
// G. Error-kind variants (LF source, error injected near the middle)
// ═══════════════════════════════════════════════════════════════════════════

fn lf_error_kind_test(error_bytes: &[u8], label: &str) {
    let content = generate(500, b"\n", usize::MAX, b"", Some(200), error_bytes);
    let out = rr2_run_cfg(&content, "TARGET000000", b"PATCHED000000", None).unwrap();
    assert_starts(&out, b"PATCHED000000\n", label);
    assert_not_contains(&out, b"TARGET000000", label);
    // The error bytes must still be present in the output (not modified by regex).
    assert_contains(&out, error_bytes, label);
}

/// G-1: `0xFF 0xFF` — definitively invalid in all single-byte and multi-byte encodings.
#[test]
fn g1_error_kind_invalid_seq() {
    lf_error_kind_test(b"\xff\xff", "G-1 invalid_seq");
}

/// G-2: `0xC2` alone — a UTF-8 two-byte lead with no continuation byte.
#[test]
fn g2_error_kind_truncated_seq() {
    lf_error_kind_test(b"\xc2", "G-2 truncated_seq");
}

/// G-3: `0xC0 0x80` — overlong encoding of NUL; invalid in strict UTF-8.
#[test]
fn g3_error_kind_overlong() {
    lf_error_kind_test(b"\xc0\x80", "G-3 overlong");
}

/// G-4: `0xED 0xA0 0x80` — U+D800 (high surrogate) encoded in UTF-8; invalid.
#[test]
fn g4_error_kind_surrogate() {
    lf_error_kind_test(b"\xed\xa0\x80", "G-4 surrogate");
}

// ═══════════════════════════════════════════════════════════════════════════
// H. Pattern placement relative to the encoding marker
// ═══════════════════════════════════════════════════════════════════════════

/// H-1: TARGET on the line immediately before the marker.
#[test]
fn h1_pattern_before_marker() {
    // First 13 bytes: "TARGET000000\n".  Marker starts at byte 13.
    let content = generate(200, b"\n", 13, b"\xa9", None, b"");
    let out = rr2_run(&content, "TARGET000000", b"PATCHED").unwrap();
    assert_starts(&out, b"PATCHED\n", "H-1 before marker");
    assert_contains(&out, b"\xa9", "H-1 marker preserved");
}

/// H-2: Pattern appears on the line immediately after the marker bytes.
#[test]
fn h2_pattern_after_marker() {
    // Inject marker at byte 0 (before any line), so first line starts right after.
    let content = generate(200, b"\n", 0, b"\xa9", None, b"");
    // The first line after marker starts at b"\xa9TARGET000000\n".
    // The regex matches "TARGET000000" even though it follows 0xA9.
    let out = rr2_run(&content, "TARGET000000", b"PATCHED").unwrap();
    assert_not_contains(&out, b"TARGET000000", "H-2 after marker");
    assert_contains(&out, b"PATCHED", "H-2 replacement present");
}

/// H-3: Pattern ONLY after the marker; ensure bytes before marker are untouched.
#[test]
fn h3_pattern_only_after_marker_prefix_clean() {
    let content = generate(200, b"\n", 0, b"\xa9", None, b"");
    let original_prefix: Vec<u8> = content.iter().take(1).cloned().collect();
    let out = rr2_run(&content, "TARGET000000", b"PATCHED").unwrap();
    // The 0xA9 byte at position 0 is not part of any match, survives unchanged.
    assert_eq!(out[0], original_prefix[0], "H-3 pre-marker byte changed");
}

// ═══════════════════════════════════════════════════════════════════════════
// I. Large files — many TARGET occurrences
// ═══════════════════════════════════════════════════════════════════════════

/// Count how many TARGET matches are replaced and verify all are replaced.
fn large_file_test(min_length: usize, line_ending: &[u8], label: &str) {
    let content = generate(min_length, line_ending, usize::MAX, b"", None, b"");
    let target_count = count_occurrences(&content, b"TARGET");
    assert!(target_count > 0, "[{label}] no TARGET in generated content");

    let out = rr2_run_cfg(&content, "TARGET", b"PATCHED", None).unwrap();
    assert_not_contains(&out, b"TARGET", label);
    let patch_count = count_occurrences(&out, b"PATCHED");
    assert_eq!(
        patch_count, target_count,
        "[{label}] expected {target_count} replacements, got {patch_count}"
    );
}

/// I-1: ~1 000-byte LF file (≈76 TARGET lines / ~7 matches at stride-10).
#[test]
fn i1_large_lf_1k() {
    large_file_test(1_000, b"\n", "I-1 LF 1kB");
}

/// I-2: ~10 000-byte LF file.
#[test]
fn i2_large_lf_10k() {
    large_file_test(10_000, b"\n", "I-2 LF 10kB");
}

/// I-3: ~100 000-byte LF file.
#[test]
fn i3_large_lf_100k() {
    large_file_test(100_000, b"\n", "I-3 LF 100kB");
}

/// I-4: ~10 000-byte CRLF file.
#[test]
fn i4_large_crlf_10k() {
    large_file_test(10_000, b"\r\n", "I-4 CRLF 10kB");
}

/// I-5: ~100 000-byte CRLF file — verifies no bare LF in output.
#[test]
fn i5_large_crlf_100k_no_bare_lf() {
    let content = generate(100_000, b"\r\n", usize::MAX, b"", None, b"");
    let out = rr2_run_cfg(&content, "TARGET", b"PATCHED", None).unwrap();
    // Every \n in output must be preceded by \r.
    for i in 0..out.len() {
        if out[i] == b'\n' {
            assert!(i > 0 && out[i - 1] == b'\r', "I-5 bare LF at offset {i}");
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// J. Regex-feature edge cases
// ═══════════════════════════════════════════════════════════════════════════

/// J-1: Start-of-text anchor `\A` — matches only the very first position.
#[test]
fn j1_start_of_text_anchor() {
    let out = rr2_run(b"foo\nfoo\n", r"\Afoo", b"FIRST").unwrap();
    assert_rr2(&out, b"FIRST\nfoo\n", "J-1 \\A anchor");
}

/// J-2: Alternation `a|b` replaces both variants.
#[test]
fn j2_alternation() {
    let out = rr2_run(b"apple\nbanana\ncherry\n", "apple|banana", b"fruit").unwrap();
    assert_rr2(&out, b"fruit\nfruit\ncherry\n", "J-2 alternation");
}

/// J-3: Quantifier `+` — greedy match of one-or-more digits.
#[test]
fn j3_plus_quantifier_greedy() {
    let out = rr2_run(b"abc123def456ghi\n", r"\d+", b"NUM").unwrap();
    assert_rr2(&out, b"abcNUMdefNUMghi\n", "J-3 \\d+ greedy");
}

/// J-4: Non-greedy `+?` stops at the first digit.
#[test]
fn j4_non_greedy_plus() {
    let out = rr2_run(b"123\n", r"\d+?", b"X").unwrap();
    assert_rr2(&out, b"XXX\n", "J-4 non-greedy +? — each digit separately");
}

/// J-5: Dot `.` in default mode does NOT match `\n`.
#[test]
fn j5_dot_does_not_match_newline() {
    let out = rr2_run(b"a\nb\n", ".", b"X").unwrap();
    assert_rr2(&out, b"X\nX\n", "J-5 dot skips newline");
}

/// J-6: `(?s)` dot-matches-all flag — `.` now matches `\n` in normalised view.
#[test]
fn j6_dot_matches_newline_with_s_flag() {
    let out = rr2_run(b"a\nb\n", "(?s).", b"X").unwrap();
    // All 4 bytes (a \n b \n) are replaced by X.
    assert_rr2(&out, b"XXXX", "J-6 (?s) dot matches LF");
}

/// J-7: Named capture group.
#[test]
fn j7_named_capture_group() {
    let out = rr2_run(b"hello world\n", r"(?P<w>\w+)", b"[$w]").unwrap();
    assert_rr2(&out, b"[hello] [world]\n", "J-7 named capture");
}

// ═══════════════════════════════════════════════════════════════════════════
// K. Replacement edge cases
// ═══════════════════════════════════════════════════════════════════════════

/// K-1: Empty replacement (deletion).
#[test]
fn k1_empty_replacement_deletes_match() {
    let out = rr2_run(b"fooXYZbar\n", "XYZ", b"").unwrap();
    assert_rr2(&out, b"foobar\n", "K-1 empty repl");
}

/// K-2: Replacement longer than the match.
#[test]
fn k2_replacement_longer_than_match() {
    let out = rr2_run(b"short\n", "short", b"much_longer_replacement").unwrap();
    assert_rr2(&out, b"much_longer_replacement\n", "K-2 longer repl");
}

/// K-3: Replace all occurrences with empty string (whole-file deletion of pattern).
#[test]
fn k3_delete_all_occurrences() {
    let out = rr2_run(b"a!b!c!\n", "!", b"").unwrap();
    assert_rr2(&out, b"abc\n", "K-3 delete all");
}

/// K-4: Replacement contains `$$` — literal dollar sign in output.
#[test]
fn k4_literal_dollar_in_replacement() {
    let out = rr2_run(b"price\n", "price", b"$$10").unwrap();
    assert_rr2(&out, b"$10\n", "K-4 literal $$");
}

/// K-5: Whole file replaced by one match spanning all content (dot-all, max repeat).
#[test]
fn k5_whole_file_replaced() {
    let out = rr2_run(b"line1\nline2\nline3\n", "(?s).*", b"REPLACED").unwrap();
    // `.*` with (?s) is greedy and matches the entire normalised content.
    assert_rr2(&out, b"REPLACED", "K-5 whole file replace");
}

// ═══════════════════════════════════════════════════════════════════════════
// L. BOM handling
// ═══════════════════════════════════════════════════════════════════════════

/// L-1: UTF-8 BOM (0xEF 0xBB 0xBF) is skipped by the Lines cursor so the
/// regex works on post-BOM content.
#[test]
fn l1_utf8_bom_skipped_in_norm_view() {
    // BOM + content.  The view cursor starts after the BOM.
    let input = b"\xef\xbb\xbffoo\nbar\n";
    let out = rr2_run(input, "foo", b"FOO").unwrap();
    // BOM bytes remain in the raw branch; only the content after the BOM
    // is in the view, but the source splice applies at the correct offset.
    assert_contains(&out, b"FOO", "L-1 BOM skipped → replacement works");
    // The BOM bytes at the start survive unchanged.
    assert_eq!(&out[..3], b"\xef\xbb\xbf", "L-1 BOM preserved in output");
}

/// L-2: Windows-1252 Euro sign (0x80) as marker; regex matches text before it.
#[test]
fn l2_win1252_euro_marker_preserved() {
    let content = generate(200, b"\n", 13, b"\x80", None, b"");
    let out = rr2_run(&content, "TARGET000000", b"X").unwrap();
    assert_contains(&out, b"\x80", "L-2 Win-1252 euro marker preserved");
    assert_not_contains(&out, b"TARGET000000", "L-2 match succeeded");
}

/// L-3: File consisting only of a UTF-8 BOM (3 bytes, no content).
#[test]
fn l3_bom_only_file() {
    let input = b"\xef\xbb\xbf";
    let out = rr2_run(input, "anything", b"X").unwrap();
    // No match possible; BOM bytes pass through unchanged.
    assert_rr2(&out, input, "L-3 BOM-only file");
}

// ═══════════════════════════════════════════════════════════════════════════
// M. Lone-CR and mixed-terminator sources
// ═══════════════════════════════════════════════════════════════════════════

/// M-1: File using lone CR (classic Mac) terminators.
#[test]
fn m1_lone_cr_terminators_preserved() {
    let input = b"foo\rbar\rbaz\r";
    let out = rr2_run(input, "bar", b"BAR").unwrap();
    assert_rr2(&out, b"foo\rBAR\rbaz\r", "M-1 lone CR");
}

/// M-2: Mixed CRLF/LF — majority is LF, dominant line ending = LF.
/// Replacement `\n` should remain `\n` (not get upgraded to CRLF).
#[test]
fn m2_mixed_mostly_lf() {
    // 3 LF lines, 1 CRLF line → majority LF detected.
    let input = b"a\nb\nc\nd\r\n";
    let out = rr2_run(input, "a", b"A").unwrap();
    // No `\n` in replacement, so DenormaliseWriter not relevant here.
    // The CRLF line must survive unchanged.
    assert_contains(&out, b"d\r\n", "M-2 CRLF line preserved");
    assert_starts(&out, b"A\n", "M-2 LF replaced correctly");
}

/// M-3: Cross-line match on lone-CR source.
#[test]
fn m3_cross_line_lone_cr() {
    let input = b"foo\rbar\rbaz\r";
    // In the normalised view each \r becomes \n, so "foo\nbar" spans two lines.
    let out = rr2_run(input, r"foo\nbar", b"SPAN").unwrap();
    assert_rr2(&out, b"SPAN\rbaz\r", "M-3 cross-line lone CR");
}

// ═══════════════════════════════════════════════════════════════════════════
// N. CRLF source with error bytes — cross-line match near error
// ═══════════════════════════════════════════════════════════════════════════

/// N-1: CRLF source; match spans the line immediately before the error injection.
#[test]
fn n1_crlf_match_before_error_injection() {
    // Generate: TARGET000000\r\n ... <error at ~100> ...
    let content = generate(500, b"\r\n", usize::MAX, b"", Some(100), b"\xff\xff");
    // TARGET000000 is on line 0, bytes 0-11; \r\n at 12-13.  Error at ~100.
    let out = rr2_run_cfg(&content, "TARGET000000", b"PATCHED", None).unwrap();
    assert_starts(&out, b"PATCHED\r\n", "N-1 CRLF match before error");
    assert_not_contains(&out, b"TARGET000000", "N-1 replacement applied");
    assert_contains(&out, b"\xff\xff", "N-1 error bytes preserved");
}

/// N-2: CRLF source; the error bytes happen to contain `\r\n` — view_range
/// normalises them to a single `\n`, but source coords map back correctly.
#[test]
fn n2_crlf_and_crlf_error_bytes() {
    // Inject a CRLF pair as the "error" (not really an encoding error, but
    // tests the offset-map drift accounting when the error region contains CRLF).
    let content = generate(500, b"\r\n", usize::MAX, b"", Some(100), b"\r\n\r\n");
    let out = rr2_run_cfg(&content, "TARGET000000", b"PATCHED", None).unwrap();
    assert_starts(&out, b"PATCHED\r\n", "N-2 CRLF within error");
    assert_not_contains(&out, b"TARGET000000", "N-2 replacement applied");
}

/// N-3: Pattern appears AFTER the error bytes in a CRLF file.
/// The error bytes at ~30 come before the next TARGET line at stride-10 * 13 = 130.
#[test]
fn n3_crlf_pattern_after_error() {
    let content = generate(500, b"\r\n", usize::MAX, b"", Some(5), b"\xff");
    // Find where TARGET000010 would appear (11th line, each CRLF line is 14 B).
    let out = rr2_run_cfg(&content, "TARGET000010", b"PATCHED_10", None).unwrap();
    assert_not_contains(&out, b"TARGET000010", "N-3 second TARGET replaced");
    assert_contains(&out, b"PATCHED_10", "N-3 replacement found");
}

/// N-4: Two separate TARGET-replace passes commute — applying the same
/// replacement twice on the output restores the same count as one pass.
#[test]
fn n4_idempotent_double_pass() {
    let content = generate(500, b"\r\n", usize::MAX, b"", Some(200), b"\xff\xff");
    let pass1 = rr2_run_cfg(&content, "TARGET", b"PATCHED", None).unwrap();
    // No more TARGETs after pass 1.
    assert_not_contains(&pass1, b"TARGET", "N-4 pass1 cleared TARGETs");
    // Second pass finds no TARGETs, output should equal pass1.
    let pass2 = rr2_run_cfg(&pass1, "TARGET", b"PATCHED", None).unwrap();
    assert_rr2(&pass2, &pass1, "N-4 idempotent double-pass");
}

// ── helper ────────────────────────────────────────────────────────────────────
