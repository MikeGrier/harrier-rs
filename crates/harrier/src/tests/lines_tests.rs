// Copyright (c) 2026, Michael Grier

//! Unit tests for `Lines`, `LinesError`, `LineTerminator`, and `TerminatorLog`
//! (MA-40).
//!
//! All tests are deterministic and use in-memory branches only.

use std::sync::Arc;

use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE};
use redwing::{Branch, make_thicket_from_bytes};

use crate::{
    encoded::Encoded,
    encoding::{BomPolicy, LineEnding, SourceConfig},
    lines::{LineTerminator, Lines, LinesError, TerminatorLog},
    source::Source,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn branch_of(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

/// Create a `Lines` forcing UTF-8 (avoids chardetng for purely binary content).
fn lines_utf8(bytes: impl Into<Vec<u8>>) -> Lines {
    let branch = branch_of(bytes);
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        bom_policy: BomPolicy::Honour,
        ..SourceConfig::default()
    };
    Source::new(branch, config).unwrap().as_lines().unwrap()
}

/// Create a `Lines` from bytes that include a UTF-8 BOM prefix.
fn lines_utf8_bom(content: impl Into<Vec<u8>>) -> Lines {
    let mut bytes = vec![0xEFu8, 0xBB, 0xBF]; // UTF-8 BOM
    bytes.extend(content.into());
    let branch = branch_of(bytes);
    Source::new(branch, SourceConfig::default())
        .unwrap()
        .as_lines()
        .unwrap()
}

/// Collect all `(bytes, terminator)` pairs from a `Lines` into a `Vec`.
fn collect(lines: Lines) -> Vec<(Vec<u8>, LineTerminator)> {
    let mut out = Vec::new();
    for item in lines {
        out.push(item);
    }
    out
}

// ── Iterator: basic cases ─────────────────────────────────────────────────────

/// 1. Empty file → iterator is exhausted immediately.
#[test]
fn empty_file_no_lines() {
    let items = collect(lines_utf8(b""));
    assert!(items.is_empty());
}

/// 2. Single line with LF terminator.
#[test]
fn single_line_with_lf() {
    let items = collect(lines_utf8(b"hello\n"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, b"hello\n");
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
}

/// 3. Single line with NO terminator → End sentinel.
#[test]
fn single_line_no_terminator() {
    let items = collect(lines_utf8(b"hello"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, b"hello");
    assert_eq!(items[0].1, LineTerminator::End);
}

/// 4. Pure-LF file with three lines.
#[test]
fn pure_lf_three_lines() {
    let items = collect(lines_utf8(b"alpha\nbeta\ngamma\n"));
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0],
        (b"alpha\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
    assert_eq!(
        items[1],
        (b"beta\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
    assert_eq!(
        items[2],
        (b"gamma\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
}

/// 5. Pure-CRLF file with three lines.
#[test]
fn pure_crlf_three_lines() {
    let items = collect(lines_utf8(b"alpha\r\nbeta\r\ngamma\r\n"));
    assert_eq!(items.len(), 3);
    // Normalised bytes: \r\n → \n in the yielded content.
    assert_eq!(
        items[0],
        (
            b"alpha\n".to_vec(),
            LineTerminator::Ending(LineEnding::CrLf)
        )
    );
    assert_eq!(
        items[1],
        (b"beta\n".to_vec(), LineTerminator::Ending(LineEnding::CrLf))
    );
    assert_eq!(
        items[2],
        (
            b"gamma\n".to_vec(),
            LineTerminator::Ending(LineEnding::CrLf)
        )
    );
}

/// 6. Pure-CR file with three lines.
#[test]
fn pure_cr_three_lines() {
    // "a\rb\rc\r" — three lines each terminated by lone CR.
    let items = collect(lines_utf8(b"a\rb\rc\r"));
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0],
        (b"a\n".to_vec(), LineTerminator::Ending(LineEnding::Cr))
    );
    assert_eq!(
        items[1],
        (b"b\n".to_vec(), LineTerminator::Ending(LineEnding::Cr))
    );
    assert_eq!(
        items[2],
        (b"c\n".to_vec(), LineTerminator::Ending(LineEnding::Cr))
    );
}

/// 7. Mixed LF + CRLF + CR terminators in one file.
#[test]
fn mixed_lf_crlf_cr() {
    // "a\nb\r\nc\r" → LF, CrLf, Cr (last byte is lone CR)
    let items = collect(lines_utf8(b"a\nb\r\nc\r"));
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0],
        (b"a\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
    assert_eq!(
        items[1],
        (b"b\n".to_vec(), LineTerminator::Ending(LineEnding::CrLf))
    );
    assert_eq!(
        items[2],
        (b"c\n".to_vec(), LineTerminator::Ending(LineEnding::Cr))
    );
}

/// 8. Final line with no terminator after a terminated line.
#[test]
fn final_line_no_terminator_after_terminated() {
    let items = collect(lines_utf8(b"line1\nline2"));
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        (b"line1\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
    assert_eq!(items[1], (b"line2".to_vec(), LineTerminator::End));
}

/// 9. Single CRLF followed by an unterminated line.
#[test]
fn single_crlf_then_partial() {
    let items = collect(lines_utf8(b"x\r\ny"));
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        (b"x\n".to_vec(), LineTerminator::Ending(LineEnding::CrLf))
    );
    assert_eq!(items[1], (b"y".to_vec(), LineTerminator::End));
}

/// 10. File containing only "\n" → one empty terminated line.
#[test]
fn file_of_single_lf() {
    let items = collect(lines_utf8(b"\n"));
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0],
        (b"\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
}

/// 11. File containing only "\r\n" → one empty terminated line.
#[test]
fn file_of_single_crlf() {
    let items = collect(lines_utf8(b"\r\n"));
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0],
        (b"\n".to_vec(), LineTerminator::Ending(LineEnding::CrLf))
    );
}

/// 12. Two consecutive empty lines (LF only).
#[test]
fn two_empty_lf_lines() {
    let items = collect(lines_utf8(b"\n\n"));
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        (b"\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
    assert_eq!(
        items[1],
        (b"\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
}

/// 13. Lone CR followed by a different line.
#[test]
fn lone_cr_followed_by_lf_line() {
    let items = collect(lines_utf8(b"a\rb\n"));
    assert_eq!(items.len(), 2);
    assert_eq!(
        items[0],
        (b"a\n".to_vec(), LineTerminator::Ending(LineEnding::Cr))
    );
    assert_eq!(
        items[1],
        (b"b\n".to_vec(), LineTerminator::Ending(LineEnding::Lf))
    );
}

// ── Iterator: long-line / chunk-boundary cases ────────────────────────────────

/// 14. Very long line with no terminator (> CHUNK = 4096 bytes).
#[test]
fn very_long_line_no_terminator() {
    let content: Vec<u8> = b"x".repeat(5000);
    let items = collect(lines_utf8(content.clone()));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, content);
    assert_eq!(items[0].1, LineTerminator::End);
}

/// 15. Very long line terminated by LF (> CHUNK bytes before the LF).
#[test]
fn very_long_line_with_lf() {
    let mut content: Vec<u8> = b"a".repeat(5000);
    content.push(b'\n');
    let items = collect(lines_utf8(content.clone()));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].0, content);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
}

/// 16. CRLF spanning a chunk boundary: 4095 bytes + "\r\n".
#[test]
fn crlf_split_across_chunk_boundary() {
    // The chunk size in the iterator is 4096. 4095 'x' bytes puts \r at
    // the very end of the first chunk; \n arrives in the next read.
    let mut content: Vec<u8> = b"x".repeat(4095);
    content.extend_from_slice(b"\r\n");

    let items = collect(lines_utf8(content));

    assert_eq!(items.len(), 1);
    // Normalised: 4095 x's + '\n' (6 bytes total = 4096)
    let mut expected = b"x".repeat(4095);
    expected.push(b'\n');
    assert_eq!(items[0].0, expected);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::CrLf));
}

/// 17. Lone CR at end of chunk followed by a non-LF byte in the next chunk.
#[test]
fn lone_cr_at_chunk_boundary() {
    // 4095 'x' bytes + CR + "y\n": the CR might be at the boundary.
    // Iterator must not misidentify this as CRLF.
    let mut content: Vec<u8> = b"x".repeat(4095);
    content.extend_from_slice(b"\ry\n");

    let items = collect(lines_utf8(content));

    assert_eq!(items.len(), 2);
    let mut first_expected = b"x".repeat(4095);
    first_expected.push(b'\n'); // lone CR normalised to \n
    assert_eq!(items[0].0, first_expected);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Cr));
    assert_eq!(items[1].0, b"y\n".to_vec());
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::Lf));
}

// ── Iterator: BOM handling ────────────────────────────────────────────────────

/// 18. UTF-8 BOM is skipped; content starts after BOM bytes.
#[test]
fn utf8_bom_is_skipped() {
    // BOM = 3 bytes; content = "hello\n"
    let items = collect(lines_utf8_bom(b"hello\n"));
    assert_eq!(items.len(), 1);
    // Must NOT include the BOM bytes in the line content.
    assert_eq!(items[0].0, b"hello\n".to_vec());
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
}

/// 19. UTF-8 BOM only (no content) → iterator is immediately exhausted.
#[test]
fn utf8_bom_only_no_content() {
    let items = collect(lines_utf8_bom(b""));
    assert!(items.is_empty());
}

// ── Iterator: cursor tracking ─────────────────────────────────────────────────

/// 20. After full iteration, cursor equals the branch length (no BOM).
#[test]
fn cursor_advances_to_eof_after_iteration() {
    let content = b"abc\ndef\n";
    let mut lines = lines_utf8(content);
    // Consume all items.
    for _ in &mut lines {}
    // cursor must be at end of file.
    assert_eq!(lines.cursor(), content.len() as u64);
}

/// 21. Cursor after iterating a CRLF file.
#[test]
fn cursor_advances_through_crlf() {
    let content = b"a\r\nb\r\n";
    let mut lines = lines_utf8(content);
    for _ in &mut lines {}
    assert_eq!(lines.cursor(), content.len() as u64);
}

// ── Encoded trait on Lines ────────────────────────────────────────────────────

/// 22. `Encoded::encoding()` returns the detected encoding.
#[test]
fn encoded_encoding_is_utf8() {
    let lines = lines_utf8(b"hello\n");
    assert_eq!(lines.encoding().name(), "UTF-8");
}

/// 23. `Encoded::line_ending()` returns the detected line-ending convention.
#[test]
fn encoded_line_ending_lf() {
    let lines = lines_utf8(b"a\nb\n");
    assert_eq!(lines.line_ending(), LineEnding::Lf);
}

/// 24. `Encoded::encode()` round-trips ASCII text.
#[test]
fn encoded_encode_ascii_roundtrip() {
    let lines = lines_utf8(b"hello\n");
    let encoded = lines.encode("hello").unwrap();
    assert_eq!(encoded, b"hello");
}

/// 25. `Encoded::branch()` returns a branch of the expected length.
#[test]
fn encoded_branch_correct_length() {
    let content = b"abc\ndef\n";
    let lines = lines_utf8(content);
    assert_eq!(lines.branch().byte_len(), content.len() as u64);
}

// ── view_range ────────────────────────────────────────────────────────────────

/// 26. `view_range` over a single LF-terminated line returns correct bytes.
#[test]
fn view_range_single_lf_line() {
    // "hello\nworld\n" — first line is bytes 0..6.
    let lines = lines_utf8(b"hello\nworld\n");
    let view = lines.view_range(0..6).unwrap();
    assert_eq!(view.bytes, b"hello\n");
}

/// 27. `view_range` spanning two LF lines.
#[test]
fn view_range_spanning_two_lf_lines() {
    let lines = lines_utf8(b"abc\ndef\n");
    let view = lines.view_range(0..8).unwrap();
    // Pure LF: normalised == raw.
    assert_eq!(view.bytes, b"abc\ndef\n");
}

/// 28. `view_range` over a CRLF range normalises to LF.
#[test]
fn view_range_crlf_normalised() {
    // "hello\r\nworld\r\n" bytes: 0..7 = "hello\r\n"
    let lines = lines_utf8(b"hello\r\nworld\r\n");
    let view = lines.view_range(0..7).unwrap();
    // The \r\n sequence is normalised to a single \n in view.bytes.
    assert_eq!(view.bytes, b"hello\n");
}

/// 29. `view_range` on a zero-length range returns an empty `View`.
#[test]
fn view_range_zero_length() {
    let lines = lines_utf8(b"hello\nworld\n");
    let view = lines.view_range(3..3).unwrap();
    assert!(view.bytes.is_empty());
}

/// 30. `view_range` exceeding the ceiling returns `RangeExceedsCeiling`.
#[test]
fn view_range_exceeds_ceiling_returns_error() {
    let lines = lines_utf8(b"hello world\n").with_view_ceiling(5);
    // Request 12 bytes → exceeds the 5-byte ceiling.
    let result = lines.view_range(0..12);
    assert!(
        matches!(
            result,
            Err(LinesError::RangeExceedsCeiling {
                requested: 12,
                ceiling: 5
            })
        ),
        "expected RangeExceedsCeiling, got {:?}",
        result.err()
    );
}

/// 31. `view_range` with custom ceiling that is not exceeded succeeds.
#[test]
fn view_range_within_custom_ceiling() {
    let lines = lines_utf8(b"hello world\n").with_view_ceiling(12);
    let view = lines.view_range(0..12).unwrap();
    assert_eq!(view.bytes, b"hello world\n");
}

// ── TerminatorLog ─────────────────────────────────────────────────────────────

/// 32. Push entries and iterate in insertion order (oldest → newest).
#[test]
fn termlog_push_and_iter_order() {
    let mut log = TerminatorLog::new(5);
    log.push(LineEnding::Lf);
    log.push(LineEnding::CrLf);
    log.push(LineEnding::Cr);
    let collected: Vec<_> = log.iter().collect();
    assert_eq!(
        collected,
        [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr]
    );
}

/// 33. `len()` and `is_empty()` are updated by `push`.
#[test]
fn termlog_len_and_is_empty() {
    let mut log = TerminatorLog::new(3);
    assert!(log.is_empty());
    assert_eq!(log.len(), 0);
    log.push(LineEnding::Lf);
    assert!(!log.is_empty());
    assert_eq!(log.len(), 1);
    log.push(LineEnding::CrLf);
    assert_eq!(log.len(), 2);
    log.push(LineEnding::Cr);
    assert_eq!(log.len(), 3);
}

/// 34. Ring overflow: oldest entries are overwritten when capacity is full.
#[test]
fn termlog_ring_overflow_drops_oldest() {
    let mut log = TerminatorLog::new(3);
    log.push(LineEnding::Lf); // will be evicted
    log.push(LineEnding::CrLf); // will be evicted
    log.push(LineEnding::Lf);
    log.push(LineEnding::CrLf);
    log.push(LineEnding::Cr);
    // Only last 3 remain; first two (Lf, CrLf) were overwritten.
    assert_eq!(log.len(), 3);
    let collected: Vec<_> = log.iter().collect();
    assert_eq!(
        collected,
        [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr]
    );
}

/// 35. Capacity-0 log is always empty and `push` is a no-op.
#[test]
fn termlog_capacity_zero_noop() {
    let mut log = TerminatorLog::new(0);
    assert_eq!(log.capacity(), 0);
    log.push(LineEnding::Lf);
    log.push(LineEnding::CrLf);
    assert_eq!(log.len(), 0);
    assert!(log.is_empty());
    assert_eq!(log.iter().count(), 0);
}

/// 36. Iterating an empty log yields nothing.
#[test]
fn termlog_iter_on_empty_log() {
    let log = TerminatorLog::new(10);
    assert_eq!(log.iter().count(), 0);
}

/// 37. Capacity is reported correctly.
#[test]
fn termlog_capacity_reported() {
    let log = TerminatorLog::new(7);
    assert_eq!(log.capacity(), 7);
}

/// 38. Push exactly up to capacity then one more → len stays at capacity.
#[test]
fn termlog_exactly_full_then_overflow() {
    let mut log = TerminatorLog::new(2);
    log.push(LineEnding::Lf);
    log.push(LineEnding::CrLf);
    assert_eq!(log.len(), 2);
    log.push(LineEnding::Cr); // overflow: Lf is dropped
    assert_eq!(log.len(), 2);
    let collected: Vec<_> = log.iter().collect();
    assert_eq!(collected, [LineEnding::CrLf, LineEnding::Cr]);
}

// ── Lines + TerminatorLog integration ────────────────────────────────────────

/// 39. Terminator log records per-line kind in correct order from a mixed file.
#[test]
fn lines_and_termlog_record_kinds_in_order() {
    let content = b"a\nb\r\nc\rd";
    let lines = lines_utf8(content);
    let mut log = TerminatorLog::new(10);

    for (_bytes, term) in lines {
        if let LineTerminator::Ending(le) = term {
            log.push(le);
        }
        // LineTerminator::End has no terminator to log.
    }

    // 3 terminated lines: Lf, CrLf, Cr; 4th line ("d") gets End.
    let kinds: Vec<_> = log.iter().collect();
    assert_eq!(kinds, [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr]);
}

/// 40. Terminator log fed to DenormaliseWriter re-applies original terminators.
#[test]
fn termlog_round_trip_through_denormalise() {
    use std::io::Write;

    use crate::denormalise::DenormaliseWriter;

    let content = b"hello\r\nworld\r\nfoo\n";
    let lines = lines_utf8(content);
    let mut log = TerminatorLog::new(10);
    let mut normalised_all = Vec::new();

    for (bytes, term) in lines {
        if let LineTerminator::Ending(le) = term {
            log.push(le);
        }
        normalised_all.extend_from_slice(&bytes);
    }

    // All normalised: "hello\nworld\nfoo\n"
    assert_eq!(normalised_all, b"hello\nworld\nfoo\n");

    // Re-apply via DenormaliseWriter.
    let mut dw = DenormaliseWriter::new(Vec::<u8>::new(), log.iter());
    dw.write_all(&normalised_all).unwrap();
    let restored = dw.finish().unwrap();

    // Should match original.
    assert_eq!(restored, content.as_ref());
}

// ── UTF-16LE iterator ─────────────────────────────────────────────────────────

/// Build a `Lines` iterator for a UTF-16LE source with a BOM.
/// `text` is encoded as UTF-16LE; a BOM ([0xFF, 0xFE]) is prepended.
fn lines_utf16le(text: &str) -> Lines {
    let mut bytes: Vec<u8> = vec![0xFF, 0xFE]; // UTF-16LE BOM
    for cu in text.encode_utf16() {
        bytes.push(cu as u8);
        bytes.push((cu >> 8) as u8);
    }
    let branch = branch_of(bytes);
    let config = SourceConfig {
        encoding_hint: Some(UTF_16LE),
        bom_policy: BomPolicy::Honour,
        ..SourceConfig::default()
    };
    Source::new(branch, config).unwrap().as_lines().unwrap()
}

/// Build a `Lines` iterator for a UTF-16BE source with a BOM.
/// `text` is encoded as UTF-16BE; a BOM ([0xFE, 0xFF]) is prepended.
fn lines_utf16be(text: &str) -> Lines {
    let mut bytes: Vec<u8> = vec![0xFE, 0xFF]; // UTF-16BE BOM
    for cu in text.encode_utf16() {
        bytes.push((cu >> 8) as u8);
        bytes.push(cu as u8);
    }
    let branch = branch_of(bytes);
    let config = SourceConfig {
        encoding_hint: Some(UTF_16BE),
        bom_policy: BomPolicy::Honour,
        ..SourceConfig::default()
    };
    Source::new(branch, config).unwrap().as_lines().unwrap()
}

/// Decode a UTF-16LE raw line slice (as yielded by `next_utf16`) back to a
/// `String` using encoding_rs, stripping any trailing `\n`.
fn decode_utf16le(raw: &[u8]) -> String {
    let (cow, _) = UTF_16LE.decode_without_bom_handling(raw);
    cow.trim_end_matches('\n').to_owned()
}

/// Decode a UTF-16BE raw line slice back to a `String`, stripping trailing `\n`.
fn decode_utf16be(raw: &[u8]) -> String {
    let (cow, _) = UTF_16BE.decode_without_bom_handling(raw);
    cow.trim_end_matches('\n').to_owned()
}

/// 41. UTF-16LE: single LF-terminated line with ASCII content.
#[test]
fn utf16le_single_lf_line_ascii() {
    let items = collect(lines_utf16le("hello\n"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(decode_utf16le(&items[0].0), "hello");
}

/// 42. UTF-16LE: single line with NO trailing newline (End sentinel).
#[test]
fn utf16le_single_line_no_terminator() {
    let items = collect(lines_utf16le("hello"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, LineTerminator::End);
    assert_eq!(decode_utf16le(&items[0].0), "hello");
}

/// 43. UTF-16LE: three LF-terminated lines.
#[test]
fn utf16le_three_lf_lines() {
    let items = collect(lines_utf16le("alpha\nbeta\ngamma\n"));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(items[2].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(decode_utf16le(&items[0].0), "alpha");
    assert_eq!(decode_utf16le(&items[1].0), "beta");
    assert_eq!(decode_utf16le(&items[2].0), "gamma");
}

/// 44. UTF-16LE: three CRLF-terminated lines.
#[test]
fn utf16le_three_crlf_lines() {
    let items = collect(lines_utf16le("alpha\r\nbeta\r\ngamma\r\n"));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(items[2].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(decode_utf16le(&items[0].0), "alpha");
    assert_eq!(decode_utf16le(&items[1].0), "beta");
    assert_eq!(decode_utf16le(&items[2].0), "gamma");
}

/// 45. UTF-16LE: lone CR terminator.
#[test]
fn utf16le_lone_cr_lines() {
    let items = collect(lines_utf16le("a\rb\r"));
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Cr));
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::Cr));
    assert_eq!(decode_utf16le(&items[0].0), "a");
    assert_eq!(decode_utf16le(&items[1].0), "b");
}

/// 46. UTF-16LE: non-ASCII Unicode character (proves decode, not byte-scan).
#[test]
fn utf16le_non_ascii_unicode() {
    // U+00F6 ö, U+4E2D 中, U+1F600 😀 (surrogate pair)
    let items = collect(lines_utf16le("caf\u{00E9}\nröd\n\u{4E2D}\u{6587}\n"));
    assert_eq!(items.len(), 3);
    assert_eq!(decode_utf16le(&items[0].0), "caf\u{00E9}");
    assert_eq!(decode_utf16le(&items[1].0), "r\u{00F6}d");
    assert_eq!(decode_utf16le(&items[2].0), "\u{4E2D}\u{6587}");
}

/// 47. UTF-16LE: surrogate pair on a line followed by LF.
#[test]
fn utf16le_surrogate_pair() {
    // U+1F600 😀 encodes as surrogate pair [0xD83D, 0xDE00] in UTF-16.
    let items = collect(lines_utf16le("\u{1F600}\n"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(decode_utf16le(&items[0].0), "\u{1F600}");
}

/// 48. UTF-16LE: empty file (BOM only) → iterator exhausted immediately.
#[test]
fn utf16le_empty_file() {
    let items = collect(lines_utf16le(""));
    assert!(items.is_empty());
}

/// 49. UTF-16LE: file of only a single LF → one empty terminated line.
#[test]
fn utf16le_file_of_single_lf() {
    let items = collect(lines_utf16le("\n"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(decode_utf16le(&items[0].0), "");
}

/// 50. UTF-16LE: mixed terminators (LF, CRLF, CR) in one file.
#[test]
fn utf16le_mixed_terminators() {
    let items = collect(lines_utf16le("a\nb\r\nc\r"));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(items[2].1, LineTerminator::Ending(LineEnding::Cr));
    assert_eq!(decode_utf16le(&items[0].0), "a");
    assert_eq!(decode_utf16le(&items[1].0), "b");
    assert_eq!(decode_utf16le(&items[2].0), "c");
}

/// 51. UTF-16LE: final partial line after a terminated line.
#[test]
fn utf16le_final_partial_after_terminated() {
    let items = collect(lines_utf16le("line1\nline2"));
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(items[1].1, LineTerminator::End);
    assert_eq!(decode_utf16le(&items[0].0), "line1");
    assert_eq!(decode_utf16le(&items[1].0), "line2");
}

/// 52. UTF-16LE: very long line (> CHUNK = 4096 bytes, ~2048 UTF-16 code units).
#[test]
fn utf16le_very_long_line() {
    let text: String = "x".repeat(2500); // 2500 chars → 5000 UTF-16LE bytes (> 4096)
    let with_newline = format!("{text}\n");
    let items = collect(lines_utf16le(&with_newline));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(decode_utf16le(&items[0].0), text);
}

/// 53. UTF-16LE: cursor after full iteration equals branch byte length.
#[test]
fn utf16le_cursor_at_eof_after_iteration() {
    // BOM (2) + "ab\n" in UTF-16LE (6 bytes) = 8 bytes total.
    let mut lines = lines_utf16le("ab\n");
    for _ in &mut lines {}
    // BOM = 2 bytes; "ab\n" = 6 bytes; branch total = 8.
    assert_eq!(lines.cursor(), 8);
}

/// 54. UTF-16LE: decoded output ends with '\n' for terminated lines, enabling
///     callers (search, head, tail) to strip it the standard way.
#[test]
fn utf16le_terminated_line_decodes_with_trailing_newline() {
    let items = collect(lines_utf16le("hello\n"));
    let (cow, _) = UTF_16LE.decode_without_bom_handling(&items[0].0);
    assert!(cow.ends_with('\n'), "decoded line must end with '\\n'");
}

// ── UTF-16BE iterator ─────────────────────────────────────────────────────────

/// 55. UTF-16BE: three LF-terminated lines with ASCII content.
#[test]
fn utf16be_three_lf_lines_ascii() {
    let items = collect(lines_utf16be("alpha\nbeta\ngamma\n"));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(items[2].1, LineTerminator::Ending(LineEnding::Lf));
    assert_eq!(decode_utf16be(&items[0].0), "alpha");
    assert_eq!(decode_utf16be(&items[1].0), "beta");
    assert_eq!(decode_utf16be(&items[2].0), "gamma");
}

/// 56. UTF-16BE: three CRLF-terminated lines.
#[test]
fn utf16be_three_crlf_lines() {
    let items = collect(lines_utf16be("x\r\ny\r\nz\r\n"));
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(items[1].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(items[2].1, LineTerminator::Ending(LineEnding::CrLf));
    assert_eq!(decode_utf16be(&items[0].0), "x");
    assert_eq!(decode_utf16be(&items[1].0), "y");
    assert_eq!(decode_utf16be(&items[2].0), "z");
}

/// 57. UTF-16BE: single line with no trailing newline → End sentinel.
#[test]
fn utf16be_single_line_no_terminator() {
    let items = collect(lines_utf16be("hello"));
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].1, LineTerminator::End);
    assert_eq!(decode_utf16be(&items[0].0), "hello");
}

/// 58. UTF-16BE: non-ASCII Unicode.
#[test]
fn utf16be_non_ascii_unicode() {
    let items = collect(lines_utf16be("r\u{00F6}d\nblå\n"));
    assert_eq!(items.len(), 2);
    assert_eq!(decode_utf16be(&items[0].0), "r\u{00F6}d");
    assert_eq!(decode_utf16be(&items[1].0), "bl\u{00E5}");
}

/// 59. UTF-16BE: decoded output ends with '\n' for terminated lines.
#[test]
fn utf16be_terminated_line_decodes_with_trailing_newline() {
    let items = collect(lines_utf16be("world\n"));
    let (cow, _) = UTF_16BE.decode_without_bom_handling(&items[0].0);
    assert!(cow.ends_with('\n'), "decoded line must end with '\\n'");
}

/// 60. UTF-16BE: empty file (BOM only).
#[test]
fn utf16be_empty_file() {
    let items = collect(lines_utf16be(""));
    assert!(items.is_empty());
}
