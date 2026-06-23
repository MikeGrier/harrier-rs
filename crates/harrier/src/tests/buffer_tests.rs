// Copyright (c) 2026, Michael Grier

//! MA-59: Unit tests for `Buffer` — `line_offset`, `offset_to_line_col`,
//! `line_content`, `view_range`, ceiling enforcement, and event delivery.

use std::sync::{Arc, mpsc};

use redwing::{branch::Branch, make_thicket_from_bytes};

use crate::{
    buffer::{Buffer, BufferError},
    encoding::SourceConfig,
    line_map_event::LineMapEvent,
    source::Source,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn branch(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

fn make_buffer(bytes: impl Into<Vec<u8>>) -> Buffer {
    let br = branch(bytes);
    Source::new(br, SourceConfig::default())
        .expect("Source::new")
        .as_buffer()
        .expect("as_buffer")
}

/// Collect *all* events from a non-blocking receiver, stopping at empty.
fn drain(rx: &mpsc::Receiver<LineMapEvent>) -> Vec<LineMapEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

// ── line_offset — LF file ─────────────────────────────────────────────────────

// 1. Line 0 of an LF file always starts at byte 0.
#[test]
fn line_offset_line0_is_zero() {
    let mut buf = make_buffer(b"alpha\nbeta\n".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
}

// 2. In-order access: line offsets increase monotonically.
#[test]
fn line_offset_in_order_lf() {
    // "line0\nline1\nline2\n"  → offsets: 0, 6, 12
    let mut buf = make_buffer(b"line0\nline1\nline2\n".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert_eq!(buf.line_offset(1).unwrap(), 6);
    assert_eq!(buf.line_offset(2).unwrap(), 12);
}

// 3. Out-of-order access: request higher line first, then lower.
//    Both must return the same result as in-order access.
#[test]
fn line_offset_out_of_order_lf() {
    let mut buf = make_buffer(b"aaa\nbbb\nccc\n".to_vec());
    // Request line 2 first (forces scanning past line 1).
    assert_eq!(buf.line_offset(2).unwrap(), 8);
    // Then request line 1.
    assert_eq!(buf.line_offset(1).unwrap(), 4);
    // Then line 0.
    assert_eq!(buf.line_offset(0).unwrap(), 0);
}

// 4. Line past EOF returns LineOutOfRange.
#[test]
fn line_offset_past_eof_returns_error() {
    // "one\ntwo" has no trailing newline → 2 lines (0, 1); line 2 is past EOF.
    let mut buf = make_buffer(b"one\ntwo".to_vec());
    let err = buf.line_offset(2).unwrap_err();
    assert!(
        matches!(err, BufferError::LineOutOfRange { line: 2, total: 2 }),
        "unexpected: {:?}",
        err
    );
}

// 5. File with no trailing newline: last line offset is computed correctly.
#[test]
fn line_offset_no_trailing_newline() {
    // "foo\nbar" → line 0 at 0, line 1 at 4, line 2 past EOF
    let mut buf = make_buffer(b"foo\nbar".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert_eq!(buf.line_offset(1).unwrap(), 4);
    let err = buf.line_offset(2).unwrap_err();
    assert!(
        matches!(err, BufferError::LineOutOfRange { line: 2, total: 2 }),
        "{err:?}"
    );
}

// 6. CRLF file: line offsets account for the two-byte terminator.
#[test]
fn line_offset_crlf_file() {
    // "ab\r\ncd\r\nef\r\n" → offsets: 0, 4, 8
    let mut buf = make_buffer(b"ab\r\ncd\r\nef\r\n".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert_eq!(buf.line_offset(1).unwrap(), 4);
    assert_eq!(buf.line_offset(2).unwrap(), 8);
}

// 7. CR-only file: line offsets account for the single-byte CR terminator.
#[test]
fn line_offset_cr_only_file() {
    // "x\ry\rz\r" → offsets: 0, 2, 4
    let mut buf = make_buffer(b"x\ry\rz\r".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert_eq!(buf.line_offset(1).unwrap(), 2);
    assert_eq!(buf.line_offset(2).unwrap(), 4);
}

// ── offset_to_line_col ────────────────────────────────────────────────────────

// 8. Byte 0 of any non-empty file → (0, 0).
#[test]
fn offset_to_line_col_start_of_file() {
    let mut buf = make_buffer(b"hello\nworld\n".to_vec());
    assert_eq!(buf.offset_to_line_col(0).unwrap(), (0, 0));
}

// 9. Byte offset at the start of the second line.
#[test]
fn offset_to_line_col_second_line_start() {
    // "hello\nworld\n": 'w' is at byte 6 → (1, 0)
    let mut buf = make_buffer(b"hello\nworld\n".to_vec());
    assert_eq!(buf.offset_to_line_col(6).unwrap(), (1, 0));
}

// 10. Mid-line offset.
#[test]
fn offset_to_line_col_mid_line() {
    // "hello\nworld\n": 'r' is at byte 9 → (1, 3)
    let mut buf = make_buffer(b"hello\nworld\n".to_vec());
    assert_eq!(buf.offset_to_line_col(9).unwrap(), (1, 3));
}

// 11. Last byte of a terminated line (the \n itself).
#[test]
fn offset_to_line_col_at_newline_byte() {
    // "ab\ncd\n": '\n' is at byte 2 → (0, 2)
    let mut buf = make_buffer(b"ab\ncd\n".to_vec());
    assert_eq!(buf.offset_to_line_col(2).unwrap(), (0, 2));
}

// 12. CRLF: column is raw source byte distance, so \r counts.
//     "ab\r\ncd\r\n": '\r' at byte 2 → (0, 2); '\n' at byte 3 → (0, 3)
#[test]
fn offset_to_line_col_crlf_raw_byte_col() {
    let mut buf = make_buffer(b"ab\r\ncd\r\n".to_vec());
    // \r is at byte 2 — still on line 0, col 2
    assert_eq!(buf.offset_to_line_col(2).unwrap(), (0, 2));
    // \n is at byte 3 — still on line 0, col 3
    assert_eq!(buf.offset_to_line_col(3).unwrap(), (0, 3));
    // 'c' is at byte 4 — first byte of line 1, col 0
    assert_eq!(buf.offset_to_line_col(4).unwrap(), (1, 0));
}

// 13. offset_to_line_col past branch end returns error.
#[test]
fn offset_to_line_col_past_eof_returns_error() {
    let mut buf = make_buffer(b"hi\n".to_vec());
    // branch length is 3; offset 3 is past the end
    let err = buf.offset_to_line_col(3).unwrap_err();
    assert!(matches!(err, BufferError::LineOutOfRange { .. }), "{err:?}");
}

// ── line_content ──────────────────────────────────────────────────────────────

// 14. LF file: line_content bytes match expected including trailing \n.
#[test]
fn line_content_lf_bytes_correct() {
    let mut buf = make_buffer(b"foo\nbar\nbaz\n".to_vec());
    let view = buf.line_content(0).unwrap();
    assert_eq!(view.bytes, b"foo\n");
    let view1 = buf.line_content(1).unwrap();
    assert_eq!(view1.bytes, b"bar\n");
    let view2 = buf.line_content(2).unwrap();
    assert_eq!(view2.bytes, b"baz\n");
}

// 15. CRLF file: normalised bytes contain \n, not \r\n.
#[test]
fn line_content_crlf_normalised() {
    let mut buf = make_buffer(b"alpha\r\nbeta\r\n".to_vec());
    let view = buf.line_content(0).unwrap();
    assert_eq!(view.bytes, b"alpha\n");
    let view1 = buf.line_content(1).unwrap();
    assert_eq!(view1.bytes, b"beta\n");
}

// 16. CR-only file: normalised bytes contain \n.
#[test]
fn line_content_cr_normalised() {
    let mut buf = make_buffer(b"x\ry\r".to_vec());
    let view = buf.line_content(0).unwrap();
    assert_eq!(view.bytes, b"x\n");
    let view1 = buf.line_content(1).unwrap();
    assert_eq!(view1.bytes, b"y\n");
}

// 17. Final line without trailing newline: view has no \n at end.
#[test]
fn line_content_no_trailing_newline_last_line() {
    let mut buf = make_buffer(b"hello\nworld".to_vec());
    let view = buf.line_content(1).unwrap();
    assert_eq!(view.bytes, b"world");
}

// 18. line_content on out-of-range line returns LineOutOfRange.
// "one" (no trailing newline) has exactly 1 line at index 0; index 1 is out of range.
#[test]
fn line_content_out_of_range() {
    let mut buf = make_buffer(b"one".to_vec());
    let err = buf.line_content(1).map(|_| ()).unwrap_err();
    assert!(matches!(err, BufferError::LineOutOfRange { .. }), "{err:?}");
}

// ── view_range ────────────────────────────────────────────────────────────────

// 19. view_range spanning 3 lines normalises correctly.
#[test]
fn view_range_three_lf_lines() {
    let data = b"line0\nline1\nline2\n";
    let buf = make_buffer(data.to_vec());
    // Cover all 18 bytes.
    let view = buf.view_range(0..18).unwrap();
    assert_eq!(view.bytes, data.as_ref());
}

// 20. view_range on a CRLF 3-line file produces normalised LF bytes.
#[test]
fn view_range_three_crlf_lines_normalised() {
    // "a\r\nb\r\nc\r\n" = 9 bytes → normalised "a\nb\nc\n" (6 bytes)
    let buf = make_buffer(b"a\r\nb\r\nc\r\n".to_vec());
    let view = buf.view_range(0..9).unwrap();
    assert_eq!(view.bytes, b"a\nb\nc\n");
}

// 21. view_range on empty range returns empty bytes.
#[test]
fn view_range_empty_range() {
    let buf = make_buffer(b"hello\n".to_vec());
    let view = buf.view_range(2..2).unwrap();
    assert!(view.bytes.is_empty());
}

// 22. view_range exceeding ceiling returns RangeExceedsCeiling.
#[test]
fn view_range_exceeds_ceiling() {
    let buf = make_buffer(b"abcdefghij".to_vec()).with_view_ceiling(4);
    let err = buf.view_range(0..10).map(|_| ()).unwrap_err();
    assert!(
        matches!(
            err,
            BufferError::RangeExceedsCeiling {
                requested: 10,
                ceiling: 4
            }
        ),
        "{err:?}"
    );
}

// 23. view_range within ceiling succeeds.
#[test]
fn view_range_within_ceiling() {
    let buf = make_buffer(b"abcdefghij".to_vec()).with_view_ceiling(10);
    let view = buf.view_range(0..4).unwrap();
    assert_eq!(view.bytes, b"abcd");
}

// ── single-line file ──────────────────────────────────────────────────────────

// 24. Single line with terminator: line 0 at 0, trailing `\n` creates empty
//     virtual line 1 at EOF; line 2 is past EOF.
#[test]
fn single_line_with_terminator() {
    let mut buf = make_buffer(b"hello\n".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    // Line 1 is the empty virtual line after the trailing \n.
    assert_eq!(buf.line_offset(1).unwrap(), 6);
    // Line 2 is past EOF.
    assert!(matches!(
        buf.line_offset(2).unwrap_err(),
        BufferError::LineOutOfRange { .. }
    ));
    assert_eq!(buf.offset_to_line_col(0).unwrap(), (0, 0));
    assert_eq!(buf.offset_to_line_col(4).unwrap(), (0, 4));
    let view = buf.line_content(0).unwrap();
    assert_eq!(view.bytes, b"hello\n");
}

// 25. Single line without terminator: line 0 at 0, line 1 is past EOF.
#[test]
fn single_line_no_terminator() {
    let mut buf = make_buffer(b"hello".to_vec());
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert!(matches!(
        buf.line_offset(1).unwrap_err(),
        BufferError::LineOutOfRange { .. }
    ));
    let view = buf.line_content(0).unwrap();
    assert_eq!(view.bytes, b"hello");
}

// ── event delivery ────────────────────────────────────────────────────────────

// 26. After scanning via line_offset, LineCountChanged and RegionExact events
//     are delivered for each segment scanned.
#[test]
fn events_delivered_on_scan() {
    let (tx, rx) = mpsc::channel();
    let mut buf = make_buffer(b"a\nb\nc\n".to_vec()).with_sender(tx);
    // Force a scan by querying line 2.
    let _ = buf.line_offset(2).unwrap();
    let events = drain(&rx);
    // At minimum one LineCountChanged and one RegionExact.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LineMapEvent::LineCountChanged { .. })),
        "expected LineCountChanged, got: {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, LineMapEvent::RegionExact { .. })),
        "expected RegionExact, got: {events:?}"
    );
}

// 27. Calling invalidate_from_byte fires MapInvalidated then LineCountChanged.
#[test]
fn events_invalidation_order() {
    let (tx, rx) = mpsc::channel();
    let mut buf = make_buffer(b"one\ntwo\nthree\n".to_vec()).with_sender(tx);
    // Scan fully.
    let _ = buf.line_offset(2).unwrap();
    drain(&rx); // clear scan events

    // Invalidate from byte 0.
    buf.line_map_mut().invalidate_from_byte(0);
    let events = drain(&rx);
    assert_eq!(
        events.len(),
        2,
        "expected exactly MapInvalidated + LineCountChanged: {events:?}"
    );
    assert!(
        matches!(events[0], LineMapEvent::MapInvalidated { from_line: 0 }),
        "{events:?}"
    );
    assert!(
        matches!(events[1], LineMapEvent::LineCountChanged { .. }),
        "{events:?}"
    );
}

// ── many-lines stress ─────────────────────────────────────────────────────────

// 28. 1000-line LF file: line_offset in-order for all 1000 lines is correct.
#[test]
fn line_offset_1000_lf_lines_in_order() {
    // Each line is "XXXXX\n" = 6 bytes; use segment_size=64 to exercise multi-segment
    let content: Vec<u8> = (0..1000).flat_map(|_| b"XXXXX\n".iter().copied()).collect();
    let mut buf = make_buffer(content);
    for i in 0..1000 {
        let expected = i as u64 * 6;
        assert_eq!(buf.line_offset(i).unwrap(), expected, "line {i}");
    }
}

// 29. 1000-line LF file: out-of-order spot-checks.
#[test]
fn line_offset_1000_lf_lines_out_of_order() {
    let content: Vec<u8> = (0..1000).flat_map(|_| b"XXXXX\n".iter().copied()).collect();
    let mut buf = make_buffer(content);
    // Jump to line 999 first to force full scan, then check earlier lines.
    assert_eq!(buf.line_offset(999).unwrap(), 999 * 6);
    assert_eq!(buf.line_offset(0).unwrap(), 0);
    assert_eq!(buf.line_offset(500).unwrap(), 500 * 6);
    assert_eq!(buf.line_offset(1).unwrap(), 6);
}

// 30. offset_to_line_col on a 5-line file at every byte position is consistent
//     with line_offset results.
#[test]
fn offset_to_line_col_consistent_with_line_offset() {
    // "aaaa\nbbbb\ncccc\ndddd\neeee\n"
    let content = b"aaaa\nbbbb\ncccc\ndddd\neeee\n";
    let mut buf = make_buffer(content.to_vec());
    // Pre-scan by requesting line 4.
    let _ = buf.line_offset(4).unwrap();

    for line in 0..5usize {
        let lstart = buf.line_offset(line).unwrap();
        // Check every byte in the 5-byte span "XXXX\n".
        for col in 0usize..5 {
            let offset = lstart + col as u64;
            let (got_line, got_col) = buf.offset_to_line_col(offset).unwrap();
            assert_eq!(got_line, line, "offset {offset}: line mismatch");
            assert_eq!(got_col, col, "offset {offset}: col mismatch");
        }
    }
}

// ── UTF-16 encoding rejection ─────────────────────────────────────────────────

// 34. Buffer rejects UTF-16LE sources because encoding_rs has no real UTF-16LE
//     encoder: edits would emit UTF-8 bytes and silently corrupt the file.
#[test]
fn utf16le_source_rejected_at_buffer_construction() {
    use crate::encoding::BomPolicy;
    use encoding_rs::UTF_16LE;

    // UTF-16LE BOM + "hi" in UTF-16LE
    let content: Vec<u8> = vec![0xFF, 0xFE, 0x68, 0x00, 0x69, 0x00];
    let config = crate::encoding::SourceConfig {
        encoding_hint: Some(UTF_16LE),
        bom_policy: BomPolicy::Honour,
        ..crate::encoding::SourceConfig::default()
    };
    let src = Source::new(branch(content), config).expect("Source::new");
    let result = src.as_buffer();
    assert!(
        matches!(
            result,
            Err(BufferError::EncodeUnsupported {
                encoding_name: "UTF-16LE"
            })
        ),
        "expected EncodeUnsupported for UTF-16LE, got: {:?}",
        result.err()
    );
}

// 35. Buffer rejects UTF-16BE sources for the same reason as UTF-16LE.
#[test]
fn utf16be_source_rejected_at_buffer_construction() {
    use crate::encoding::BomPolicy;
    use encoding_rs::UTF_16BE;

    // UTF-16BE BOM + "hi" in UTF-16BE
    let content: Vec<u8> = vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69];
    let config = crate::encoding::SourceConfig {
        encoding_hint: Some(UTF_16BE),
        bom_policy: BomPolicy::Honour,
        ..crate::encoding::SourceConfig::default()
    };
    let src = Source::new(branch(content), config).expect("Source::new");
    let result = src.as_buffer();
    assert!(
        matches!(
            result,
            Err(BufferError::EncodeUnsupported {
                encoding_name: "UTF-16BE"
            })
        ),
        "expected EncodeUnsupported for UTF-16BE, got: {:?}",
        result.err()
    );
}

// ── Shift_JIS CRLF normalisation ─────────────────────────────────────────────

/// Helper: build a Buffer backed by a Shift_JIS-encoded branch.
/// Shift_JIS is ASCII-compatible (0x0D/0x0A appear only as standalone bytes),
/// so view_range and line_content must still normalise CR/CRLF to LF.
fn make_shift_jis_buffer(bytes: impl Into<Vec<u8>>) -> Buffer {
    let br = branch(bytes);
    let config = crate::encoding::SourceConfig {
        encoding_hint: Some(encoding_rs::SHIFT_JIS),
        ..crate::encoding::SourceConfig::default()
    };
    Source::new(br, config)
        .expect("Source::new")
        .as_buffer()
        .expect("as_buffer")
}

// 36. view_range on a Shift_JIS CRLF source normalises CR+LF → LF.
//     0x82 0xA0 is Shift_JIS 'あ'; 0x0D 0x0A is CRLF.
#[test]
fn view_range_shift_jis_crlf_normalised() {
    // "あ\r\nB\r\n" in Shift_JIS bytes
    let content: Vec<u8> = vec![0x82, 0xA0, 0x0D, 0x0A, 0x42, 0x0D, 0x0A];
    let buf = make_shift_jis_buffer(content);
    let view = buf.view_range(0..7).unwrap();
    // Normalised: 0x82 0xA0 → 'あ' (unchanged), \r\n → \n, 0x42 → 'B', \r\n → \n
    assert_eq!(view.bytes, &[0x82, 0xA0, 0x0A, 0x42, 0x0A]);
}

// 37. line_content on a Shift_JIS CRLF source normalises CR+LF → LF.
#[test]
fn line_content_shift_jis_crlf_normalised() {
    // "あ\r\n" in Shift_JIS (3 source bytes + CRLF = 4 bytes)
    // followed by "B\r\n" (1 + 2 = 3 bytes)
    let content: Vec<u8> = vec![0x82, 0xA0, 0x0D, 0x0A, 0x42, 0x0D, 0x0A];
    let mut buf = make_shift_jis_buffer(content);
    let view0 = buf.line_content(0).unwrap();
    // Normalised line 0: 0x82 0xA0 \n
    assert_eq!(view0.bytes, &[0x82, 0xA0, 0x0A]);
    let view1 = buf.line_content(1).unwrap();
    // Normalised line 1: 0x42 \n
    assert_eq!(view1.bytes, &[0x42, 0x0A]);
}

// 38. view_range on a Shift_JIS lone-CR source normalises bare CR → LF.
#[test]
fn view_range_shift_jis_bare_cr_normalised() {
    // "X\rY" in Shift_JIS (ASCII bytes, lone CR)
    let content: Vec<u8> = b"X\rY".to_vec();
    let buf = make_shift_jis_buffer(content);
    let view = buf.view_range(0..3).unwrap();
    assert_eq!(view.bytes, b"X\nY");
}
