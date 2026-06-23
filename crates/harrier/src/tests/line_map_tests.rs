// Copyright (c) 2026, Michael Grier

//! MA-49: Unit tests for `LineMap`, `LineCount`, and `LineMapSegment`.

use std::sync::{Arc, mpsc};

use redwing::{branch::Branch, make_thicket_from_bytes};

use crate::{
    encoding::LineEnding, line_count::LineCount, line_map::LineMap, line_map_event::LineMapEvent,
    line_map_segment::LineMapSegment,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn branch(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

/// Construct a `LineMap` with a custom segment size and no event sender.
fn map_no_sender(bytes: impl Into<Vec<u8>>, seg: u64) -> LineMap {
    LineMap::new(branch(bytes), Some(seg), None)
}

/// Scan all segments of a map, returning the final exact line count.
fn scan_all(map: &mut LineMap) -> LineCount {
    while map.scan_next_segment().expect("scan_next_segment failed") {}
    map.current_line_count()
}

// ── LineCount display ─────────────────────────────────────────────────────────

// 1. Exact count displays without tilde.
#[test]
fn line_count_exact_display() {
    assert_eq!(LineCount::exact(42).to_string(), "42");
}

// 2. Estimated count displays with a leading tilde.
#[test]
fn line_count_estimate_display() {
    assert_eq!(LineCount::estimate(42).to_string(), "~42");
}

// 3. Zero exact.
#[test]
fn line_count_exact_zero_display() {
    assert_eq!(LineCount::exact(0).to_string(), "0");
}

// 4. Exact/estimate equality check.
#[test]
fn line_count_exact_ne_estimate() {
    assert_ne!(LineCount::exact(10), LineCount::estimate(10));
}

// ── LineMapSegment ────────────────────────────────────────────────────────────

// 5. Unscanned segment has zero line count.
#[test]
fn segment_unscanned_line_count_zero() {
    let seg = LineMapSegment::unscanned(0..1024);
    assert_eq!(seg.line_count(), 0);
    assert!(!seg.exact);
    assert!(!seg.trailing_partial);
}

// 6. Segment with terminators and no partial tail.
#[test]
fn segment_line_count_terminators_only() {
    let mut seg = LineMapSegment::unscanned(0..10);
    seg.terminators = vec![LineEnding::Lf, LineEnding::Lf];
    seg.trailing_partial = false;
    assert_eq!(seg.line_count(), 2);
}

// 7. Segment with terminators and a partial tail.
#[test]
fn segment_line_count_with_partial_tail() {
    let mut seg = LineMapSegment::unscanned(0..10);
    seg.terminators = vec![LineEnding::Lf];
    seg.trailing_partial = true;
    // line_count() counts only terminated lines; trailing_partial is tracked
    // separately at the map level (added to total only for the last segment).
    assert_eq!(seg.line_count(), 1);
}

// 8. byte_len() matches range.
#[test]
fn segment_byte_len() {
    let seg = LineMapSegment::unscanned(16..48);
    assert_eq!(seg.byte_len(), 32);
}

// ── Empty file ────────────────────────────────────────────────────────────────

// 9. Empty file produces zero segments.
#[test]
fn empty_file_zero_segments() {
    let map = map_no_sender(b"".to_vec(), 64);
    assert_eq!(map.segment_count(), 0);
}

// 10. Scanning an empty file's (non-existent) segments immediately returns false.
#[test]
fn empty_file_scan_returns_false() {
    let mut map = map_no_sender(b"".to_vec(), 64);
    assert!(!map.scan_next_segment().expect("scan failed"));
}

// 11. Empty file has exact line count 0.
#[test]
fn empty_file_line_count_exact_zero() {
    let mut map = map_no_sender(b"".to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(0));
}

// ── Single line ───────────────────────────────────────────────────────────────

// 12. Single line no trailing newline.
#[test]
fn single_line_no_trailing_newline() {
    let mut map = map_no_sender(b"hello".to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(1));
    assert!(map.segment(0).trailing_partial);
    assert_eq!(map.segment(0).terminators, vec![]);
}

// 13. Single line with LF terminator.
#[test]
fn single_line_lf() {
    let mut map = map_no_sender(b"hello\n".to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(1));
    assert_eq!(map.segment(0).terminators, vec![LineEnding::Lf]);
    assert!(!map.segment(0).trailing_partial);
}

// 14. Single line with CRLF terminator.
#[test]
fn single_line_crlf() {
    let mut map = map_no_sender(b"hello\r\n".to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(1));
    assert_eq!(map.segment(0).terminators, vec![LineEnding::CrLf]);
}

// 15. Single line with lone-CR terminator.
#[test]
fn single_line_cr() {
    let mut map = map_no_sender(b"hello\r".to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(1));
    assert_eq!(map.segment(0).terminators, vec![LineEnding::Cr]);
}

// ── Multi-line files ──────────────────────────────────────────────────────────

// 16. Pure-LF multi-line file.
#[test]
fn multi_line_lf() {
    let data = b"foo\nbar\nbaz\n";
    let mut map = map_no_sender(data.to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(3));
    assert_eq!(
        map.segment(0).terminators,
        vec![LineEnding::Lf, LineEnding::Lf, LineEnding::Lf]
    );
}

// 17. Pure-CRLF multi-line file.
#[test]
fn multi_line_crlf() {
    let data = b"foo\r\nbar\r\nbaz\r\n";
    let mut map = map_no_sender(data.to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(3));
    assert_eq!(
        map.segment(0).terminators,
        vec![LineEnding::CrLf, LineEnding::CrLf, LineEnding::CrLf]
    );
}

// 18. Pure-CR multi-line file.
#[test]
fn multi_line_cr() {
    let data = b"foo\rbar\rbaz\r";
    let mut map = map_no_sender(data.to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(3));
    assert_eq!(
        map.segment(0).terminators,
        vec![LineEnding::Cr, LineEnding::Cr, LineEnding::Cr]
    );
}

// 19. Mixed terminators file.
#[test]
fn mixed_terminators() {
    let data = b"a\nb\r\nc\rd\n";
    let mut map = map_no_sender(data.to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(4));
    assert_eq!(
        map.segment(0).terminators,
        vec![
            LineEnding::Lf,
            LineEnding::CrLf,
            LineEnding::Cr,
            LineEnding::Lf
        ]
    );
}

// 20. File with no trailing newline and multiple lines.
#[test]
fn multi_line_no_trailing_newline() {
    let data = b"foo\nbar\nbaz";
    let mut map = map_no_sender(data.to_vec(), 64);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(3));
    assert!(map.segment(0).trailing_partial);
}

// ── Dense file (1000+ lines) ──────────────────────────────────────────────────

// 21. Dense LF file with 1000 lines.
#[test]
fn dense_lf_1000_lines() {
    let line = b"x\n";
    let data: Vec<u8> = line.repeat(1000);
    let mut map = map_no_sender(data, 256);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(1000));
}

// 22. Dense CRLF file with 1000 lines.
#[test]
fn dense_crlf_1000_lines() {
    let line = b"x\r\n";
    let data: Vec<u8> = line.repeat(1000);
    let mut map = map_no_sender(data, 256);
    let count = scan_all(&mut map);
    assert_eq!(count, LineCount::exact(1000));
}

// ── scan_next_segment advances correctly ─────────────────────────────────────

// 23. next_unscanned advances through segments in order.
#[test]
fn scan_next_segment_advances_in_order() {
    let data = b"ab\ncd\nef\n"; // 9 bytes
    let mut map = map_no_sender(data.to_vec(), 3); // 3 segments of 3 bytes each
    assert_eq!(map.segment_count(), 3);
    assert_eq!(map.next_unscanned(), Some(0));
    map.scan_next_segment().expect("scan failed");
    assert_eq!(map.next_unscanned(), Some(1));
    map.scan_next_segment().expect("scan failed");
    assert_eq!(map.next_unscanned(), Some(2));
    map.scan_next_segment().expect("scan failed");
    assert_eq!(map.next_unscanned(), None);
}

// 24. scan_next_segment returns false when all exact.
#[test]
fn scan_next_segment_false_when_all_exact() {
    let data = b"line\n";
    let mut map = map_no_sender(data.to_vec(), 64);
    assert!(map.scan_next_segment().expect("scan failed"));
    assert!(!map.scan_next_segment().expect("scan failed"));
}

// ── Estimate before any segment scanned ──────────────────────────────────────

// 25. Estimate is returned before scans.
#[test]
fn estimate_before_scan() {
    let data: Vec<u8> = b"a\n".repeat(200); // 400 bytes, 200 lines
    let map = map_no_sender(data, 256);
    let count = map.current_line_count();
    assert!(!count.exact, "should be an estimate before any scan");
    assert!(count.value > 0, "estimate should be positive");
}

// 26. Estimate before scan uses fallback density (1 per 40 bytes) for large files.
#[test]
fn estimate_before_scan_fallback_density() {
    // 400 bytes, fallback: ceiling(400/40) = 10
    let data: Vec<u8> = vec![b'x'; 400];
    let map = map_no_sender(data, 256);
    let count = map.current_line_count();
    assert!(!count.exact);
    assert_eq!(count.value, 10);
}

// ── Estimate mid-scan ─────────────────────────────────────────────────────────

// 27. Estimate mid-scan is based on scanned density.
#[test]
fn estimate_mid_scan() {
    // 2 segments of 256 bytes each; first segment has ~10 LF lines (10 bytes each)
    // then the second segment is unscanned.
    let first: Vec<u8> = {
        let mut v = Vec::new();
        // Build exactly 256 bytes: 25 lines of 10 bytes + 1 line of 6 bytes.
        for _ in 0..25 {
            v.extend_from_slice(b"xxxxxxxxx\n"); // 10 bytes * 25 = 250
        }
        v.extend_from_slice(b"xxxxx\n"); // 6 more = 256
        v
    };
    assert_eq!(first.len(), 256);
    let second: Vec<u8> = vec![b'x'; 256]; // 256 bytes, no newlines
    let mut data = first;
    data.extend(second);
    assert_eq!(data.len(), 512);

    let mut map = map_no_sender(data, 256);
    // Scan only the first segment.
    map.scan_next_segment().expect("scan failed");
    let count = map.current_line_count();
    // First 256 bytes have 26 lines → density = 26/256 → estimate for 512 bytes = 52.
    assert!(!count.exact);
    assert_eq!(count.value, 52);
}

// ── Exact after full scan ─────────────────────────────────────────────────────

// 28. After scanning all segments the count is exact.
#[test]
fn exact_after_full_scan() {
    let data: Vec<u8> = b"line\n".repeat(50);
    let mut map = map_no_sender(data, 64);
    let count = scan_all(&mut map);
    assert!(count.exact);
    assert_eq!(count.value, 50);
}

// ── Per-line terminator kind stored and retrieved ─────────────────────────────

// 29. Per-line terminators accessible through segment().
#[test]
fn per_line_terminator_kinds() {
    // 4 lines: LF, CRLF, CR, LF — in a single segment.
    let data = b"a\nb\r\nc\rd\n";
    let mut map = map_no_sender(data.to_vec(), 64);
    scan_all(&mut map);
    let terms = &map.segment(0).terminators;
    assert_eq!(terms[0], LineEnding::Lf);
    assert_eq!(terms[1], LineEnding::CrLf);
    assert_eq!(terms[2], LineEnding::Cr);
    assert_eq!(terms[3], LineEnding::Lf);
}

// 30. Terminators in a dense CRLF file are all CrLf.
#[test]
fn dense_crlf_terminators_all_crlf() {
    let data: Vec<u8> = b"x\r\n".repeat(10);
    let mut map = map_no_sender(data, 64);
    scan_all(&mut map);
    assert!(
        map.segment(0)
            .terminators
            .iter()
            .all(|&t| t == LineEnding::CrLf)
    );
}

// ── Invalidation ─────────────────────────────────────────────────────────────

// 31. Invalidation resets the correct tail of segments.
#[test]
fn invalidation_resets_tail_segments() {
    let data: Vec<u8> = b"ab\n".repeat(10); // 30 bytes, 3 bytes/seg → 10 segments
    let mut map = map_no_sender(data, 3);
    scan_all(&mut map);
    // All segments exact.
    assert_eq!(map.next_unscanned(), None);
    // Invalidate from byte 12 → segment 4 onwards.
    map.invalidate_from_byte(12);
    // Segments 0–3 remain exact, 4–9 are reset.
    for i in 0..4 {
        assert!(map.segment(i).exact, "segment {i} should still be exact");
    }
    for i in 4..10 {
        assert!(!map.segment(i).exact, "segment {i} should be reset");
    }
}

// 32. Invalidation from_line is the first line of the invalidated segment.
#[test]
fn invalidation_from_line_correct() {
    // 3 bytes/seg: "ab\n" × 10 → each segment has 1 line.
    let data: Vec<u8> = b"ab\n".repeat(10);
    let mut map = map_no_sender(data, 3);
    scan_all(&mut map);
    // Invalidate from byte 6 → segment index 2.  Lines 0,1 in segs 0,1 are kept.
    let from_line = map.invalidate_from_byte(6);
    assert_eq!(from_line, 2);
}

// 33. Invalidation fires MapInvalidated then LineCountChanged events.
#[test]
fn invalidation_events_order() {
    let (tx, rx) = mpsc::channel::<LineMapEvent>();
    let data: Vec<u8> = b"ab\n".repeat(4); // 12 bytes, 4 segs of 3
    let mut map = LineMap::new(branch(data), Some(3), Some(tx));
    scan_all(&mut map);
    // Drain scan events.
    while rx.try_recv().is_ok() {}

    map.invalidate_from_byte(6); // invalidates segments 2 and 3
    let ev1 = rx.recv().unwrap();
    let ev2 = rx.recv().unwrap();
    assert!(matches!(ev1, LineMapEvent::MapInvalidated { from_line: 2 }));
    assert!(matches!(ev2, LineMapEvent::LineCountChanged { .. }));
    assert!(rx.try_recv().is_err(), "no extra events");
}

// ── Channel event order (scan) ────────────────────────────────────────────────

// 34. Scanning fires LineCountChanged then RegionExact.
#[test]
fn scan_events_order() {
    let (tx, rx) = mpsc::channel::<LineMapEvent>();
    let data = b"foo\nbar\n"; // 8 bytes, 1 segment with seg_size=64
    let mut map = LineMap::new(branch(data.to_vec()), Some(64), Some(tx));
    map.scan_next_segment().expect("scan failed");
    let ev1 = rx.recv().unwrap();
    let ev2 = rx.recv().unwrap();
    assert!(matches!(ev1, LineMapEvent::LineCountChanged { .. }));
    assert!(
        matches!(
            ev2,
            LineMapEvent::RegionExact {
                start_line: 0,
                end_line: 1
            }
        ),
        "unexpected event: {ev2:?}"
    );
}

// 35. RegionExact carries correct start/end lines across segments.
#[test]
fn scan_region_exact_lines() {
    let (tx, rx) = mpsc::channel::<LineMapEvent>();
    // "a\nb\nc\n" = 6 bytes; 2 segments of 3 bytes each.
    // Seg 0: "a\n" + "b" → actually seg 0 is bytes 0..3 = "a\nb", 1 terminated line, 1 partial.
    // Let me redo: "a\nb\nc\n" with seg_size=2:
    //   seg 0: bytes 0..2 = "a\n"  → 1 line (Lf), no partial
    //   seg 1: bytes 2..4 = "b\n"  → 1 line (Lf), no partial
    //   seg 2: bytes 4..6 = "c\n"  → 1 line (Lf), no partial
    let data = b"a\nb\nc\n";
    let mut map = LineMap::new(branch(data.to_vec()), Some(2), Some(tx));

    // Scan segment 0 and collect events.
    map.scan_next_segment().expect("scan failed");
    let _cnt0 = rx.recv().unwrap(); // LineCountChanged
    let reg0 = rx.recv().unwrap(); // RegionExact
    assert!(
        matches!(
            reg0,
            LineMapEvent::RegionExact {
                start_line: 0,
                end_line: 0
            }
        ),
        "expected RegionExact(0,0), got {reg0:?}"
    );

    // Scan segment 1.
    map.scan_next_segment().expect("scan failed");
    let _cnt1 = rx.recv().unwrap();
    let reg1 = rx.recv().unwrap();
    assert!(
        matches!(
            reg1,
            LineMapEvent::RegionExact {
                start_line: 1,
                end_line: 1
            }
        ),
        "expected RegionExact(1,1), got {reg1:?}"
    );

    // Scan segment 2.
    map.scan_next_segment().expect("scan failed");
    let _cnt2 = rx.recv().unwrap();
    let reg2 = rx.recv().unwrap();
    assert!(
        matches!(
            reg2,
            LineMapEvent::RegionExact {
                start_line: 2,
                end_line: 2
            }
        ),
        "expected RegionExact(2,2), got {reg2:?}"
    );
}

// ── Segment boundary CRLF ─────────────────────────────────────────────────────

// 36. CRLF exactly at segment boundary is attributed to the first segment.
#[test]
fn crlf_at_segment_boundary() {
    // "abc\r\ndef\n" = 10 bytes; with seg_size=5:
    //   seg 0: bytes 0..5 = "abc\r\n"  → peek at byte 5 = '\n' wait that's not right
    //   Actually "abc\r\n" = a=0,b=1,c=2,\r=3,\n=4 → CRLF is within segment 0.
    // Let's use seg_size=4: bytes 0..4 = "abc\r", bytes 4..10 = "\ndef\n"
    //   seg 0 scan: a,b,c → partial; \r at end → peek byte 4 = '\n' → CrLf; next_skip on seg 1
    //   seg 1: skip first byte (\n at 4), scan "def\n" at bytes 5..10 → 1 LF line; trailing ok
    let data = b"abc\r\ndef\n";
    let mut map = map_no_sender(data.to_vec(), 4);
    scan_all(&mut map);
    assert_eq!(map.segment_count(), 3); // bytes: 0..4, 4..8, 8..9
    // Seg 0: CrLf (cross-boundary), no partial
    assert_eq!(map.segment(0).terminators, vec![LineEnding::CrLf]);
    assert!(!map.segment(0).trailing_partial);
    // Seg 1: "def\n" minus the leading \n (skipped) = "def\n" → Lf
    // Wait: seg 1 = bytes 4..8 = "\ndef", skip first byte (4,'\n'), scan bytes 5..8 = "def" → partial
    // Hmm, let me recalculate.
    // Actually data.len() = 9. seg_size=4:
    //   seg 0: 0..4 (4 bytes)
    //   seg 1: 4..8 (4 bytes)
    //   seg 2: 8..9 (1 byte)
    // seg 1 skip_first_byte=true → scan from byte 5 to 8 = "def" → no terminator, partial
    // seg 2: byte 8 = '\n' → Lf terminator, no partial
    assert_eq!(map.segment(1).terminators, vec![]);
    assert!(map.segment(1).trailing_partial); // "def" partial
    assert_eq!(map.segment(2).terminators, vec![LineEnding::Lf]);
    // Total lines: 2 (CrLf "abc" + Lf "def")
    assert_eq!(map.current_line_count(), LineCount::exact(2));
}

// 37. \r at segment boundary NOT followed by \n is a lone CR.
#[test]
fn lone_cr_at_segment_boundary() {
    // "abc\rdef\n" = 9 bytes; seg_size=4:
    //   seg 0: 0..4 = "abc\r"; last byte is \r; peek byte 4 = 'd' ≠ '\n' → lone Cr
    //   seg 1: 4..8 = "def\n"; no skip; processes normally → Lf
    //   seg 2: 8..9 (none)  — wait data.len()=9, seg 2 = 8..9 = "\n" hmm let me recount.
    // "abc\rdef\n" → a=0,b=1,c=2,\r=3,d=4,e=5,f=6,\n=7. Length=8.
    // seg_size=4: seg 0 = 0..4 = "abc\r"; seg 1 = 4..8 = "def\n"
    let data = b"abc\rdef\n";
    let mut map = map_no_sender(data.to_vec(), 4);
    scan_all(&mut map);
    assert_eq!(map.segment_count(), 2);
    // Seg 0: lone Cr at boundary; no partial (Cr terminates line)
    assert_eq!(map.segment(0).terminators, vec![LineEnding::Cr]);
    assert!(!map.segment(0).trailing_partial);
    assert!(!map.segment(1).skip_first_byte);
    // Seg 1: "def\n" → Lf
    assert_eq!(map.segment(1).terminators, vec![LineEnding::Lf]);
    // Total: 2 lines
    assert_eq!(map.current_line_count(), LineCount::exact(2));
}

// 38. Multiple consecutive CRLF pairs, one crossing boundary.
#[test]
fn multiple_crlf_one_crossing_boundary() {
    // "first\r\nsecond\r\nthird\n" = 22 bytes; seg_size=7:
    //   seg 0: 0..7   = "first\r\n" → CrLf (entirely within)
    //   seg 1: 7..14  = "second\r" → last byte \r, peek='\n' at 14 → CrLf cross-boundary, skip
    //   seg 2: 14..21 = "\nthird\n" → skip first byte (\n), "third\n" → Lf
    //   seg 3: 21..22 = wait, 22 bytes total, seg 3 = 21..22
    // Let me count: "first\r\nsecond\r\nthird\n"
    //   f=0,i=1,r=2,s=3,t=4,\r=5,\n=6,s=7,e=8,c=9,o=10,n=11,d=12,\r=13,\n=14,t=15,h=16,i=17,r=18,d=19,\n=20
    //   Length = 21. seg_size=7:
    //   seg 0: 0..7 = "first\r\n" → CrLf wholly within
    //   seg 1: 7..14 = "second\r" → last byte \r at 13, peek 14='\n' → CrLf cross-boundary, next_skip
    //   seg 2: 14..21 = "\nthird\n" → skip byte 14 (\n), scan 15..21 = "third\n" → Lf
    let data = b"first\r\nsecond\r\nthird\n";
    assert_eq!(data.len(), 21);
    let mut map = map_no_sender(data.to_vec(), 7);
    scan_all(&mut map);
    assert_eq!(map.segment_count(), 3);
    assert_eq!(map.segment(0).terminators, vec![LineEnding::CrLf]);
    assert_eq!(map.segment(1).terminators, vec![LineEnding::CrLf]);
    assert!(map.segment(2).skip_first_byte);
    assert_eq!(map.segment(2).terminators, vec![LineEnding::Lf]);
    assert_eq!(map.current_line_count(), LineCount::exact(3));
}

// ── Invalidation count correctness ───────────────────────────────────────────

// 39. After invalidation, line count estimate accounts for reset segments.
#[test]
fn invalidation_count_after_reset() {
    let data: Vec<u8> = b"ab\n".repeat(6); // 18 bytes, seg_size=3 → 6 segs of 1 line each
    let mut map = map_no_sender(data, 3);
    scan_all(&mut map);
    assert_eq!(map.current_line_count(), LineCount::exact(6));
    // Reset segments 3..5 (from byte 9).
    map.invalidate_from_byte(9);
    // Now segs 0-2 exact (3 lines), segs 3-5 unscanned.
    // Estimate: 3 lines in 9 bytes, 18 total bytes → estimate = 6.
    let count = map.current_line_count();
    assert!(!count.exact);
    assert_eq!(count.value, 6);
}

// 40. After re-scanning after invalidation, count is exact again.
#[test]
fn rescan_after_invalidation_is_exact() {
    let data: Vec<u8> = b"ab\n".repeat(6);
    let mut map = map_no_sender(data, 3);
    scan_all(&mut map);
    map.invalidate_from_byte(9);
    scan_all(&mut map);
    assert_eq!(map.current_line_count(), LineCount::exact(6));
}
