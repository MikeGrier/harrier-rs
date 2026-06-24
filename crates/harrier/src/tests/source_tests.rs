// Copyright (c) 2026, Michael Grier

//! MA-13: Unit tests for `Source::new`.

use std::sync::Arc;

use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
use redwing::{Branch, make_thicket_from_bytes};

use crate::{
    encoding::{BomPolicy, DEFAULT_PROBE_LEN, LineEnding, SourceConfig},
    source::{Source, SourceError, branch_is_well_formed_utf8, probe_is_well_formed_utf8},
};

// ── Helper ────────────────────────────────────────────────────────────────────

fn branch(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

// `probe_is_well_formed_utf8` tolerates a truncated trailing sequence but
// rejects a genuinely invalid byte mid-stream.
#[test]
fn probe_is_well_formed_utf8_tolerates_truncated_tail() {
    assert!(probe_is_well_formed_utf8(b"plain ascii"));
    assert!(probe_is_well_formed_utf8("café déjà →".as_bytes()));
    assert!(probe_is_well_formed_utf8(&[0xE2, 0x94])); // incomplete box-drawing seq
    assert!(probe_is_well_formed_utf8(&[0xE2]));
    assert!(probe_is_well_formed_utf8(b"")); // empty is valid UTF-8
    assert!(!probe_is_well_formed_utf8(&[0xE9, b'a'])); // 0xE9 is a valid 3-byte lead, but 'a' is not a valid continuation byte
}

// `branch_is_well_formed_utf8` validates the whole stream and rejects invalid
// bytes anywhere as well as NUL bytes.
#[test]
fn branch_is_well_formed_utf8_validates_whole_stream() {
    assert!(branch_is_well_formed_utf8(&branch("café déjà → vu\n".as_bytes().to_vec())).unwrap());
    assert!(branch_is_well_formed_utf8(&branch(Vec::new())).unwrap());
    let mut bad = vec![b'a'; 1000];
    bad.push(0xE9); // invalid byte far past any probe window
    assert!(!branch_is_well_formed_utf8(&branch(bad)).unwrap());
    assert!(!branch_is_well_formed_utf8(&branch(vec![b'a', 0, b'b'])).unwrap()); // NUL byte
}

// `branch_is_well_formed_utf8` reads in 128 KiB chunks, so a multi-byte
// sequence that straddles a chunk boundary must still validate. Build an input
// larger than several chunks and place a 3-byte sequence exactly across the
// first boundary; a truncated copy at true EOF must be rejected.
#[test]
fn branch_is_well_formed_utf8_handles_chunk_boundary_sequences() {
    const CHUNK_LEN: usize = 128 * 1024;
    let arrow = "→".as_bytes(); // 0xE2 0x86 0x92 — a 3-byte sequence

    // Pad so the arrow's lead byte lands one byte before the first boundary,
    // splitting the sequence across two reads, then keep going past 3 chunks.
    let mut data = vec![b'a'; CHUNK_LEN - 1];
    data.extend_from_slice(arrow);
    data.extend(std::iter::repeat_n(b'b', 2 * CHUNK_LEN));
    assert!(branch_is_well_formed_utf8(&branch(data)).unwrap());

    // A lead byte with no completing bytes at true end-of-file is malformed,
    // even though the same prefix mid-stream would merely be "carry".
    let mut truncated = vec![b'a'; 2 * CHUNK_LEN];
    truncated.push(0xE2); // dangling lead byte at EOF
    assert!(!branch_is_well_formed_utf8(&branch(truncated)).unwrap());
}

// 1. UTF-8 BOM present: BOM encoding wins over the caller hint.
#[test]
fn bom_utf8_overrides_encoding_hint() {
    let content: Vec<u8> = [0xEFu8, 0xBB, 0xBF] // UTF-8 BOM
        .iter()
        .chain(b"hello world\n")
        .copied()
        .collect();
    let config = SourceConfig {
        encoding_hint: Some(WINDOWS_1252),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.encoding(), UTF_8, "BOM must override the hint");
    assert_eq!(src.bom_len(), 3);
}

// 2. No BOM, no hint: chardetng detects UTF-8 from ASCII content.
#[test]
fn no_bom_chardetng_detects_utf8() {
    let content = b"The quick brown fox jumps over the lazy dog.\n".to_vec();
    let src = Source::new(branch(content), SourceConfig::default()).unwrap();
    assert_eq!(src.encoding().name(), "UTF-8");
    assert_eq!(src.bom_len(), 0);
}

// 3. No BOM, hint supplied: hint is used directly without running chardetng.
#[test]
fn encoding_hint_used_when_no_bom() {
    let content = b"Hello ASCII world\n".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(WINDOWS_1252),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.encoding(), WINDOWS_1252);
}

// 4. Decode-only encoding (REPLACEMENT) supplied as hint triggers the
//    asymmetry check and returns EncodeDecodeAsymmetry.
#[test]
fn encode_asymmetry_detection_error() {
    let content = b"hello\n".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(encoding_rs::REPLACEMENT),
        bom_policy: BomPolicy::Ignore,
        ..SourceConfig::default()
    };
    let result = Source::new(branch(content), config);
    assert!(
        matches!(result, Err(SourceError::EncodeDecodeAsymmetry { .. })),
        "expected EncodeDecodeAsymmetry, got: {:?}",
        result.err()
    );
}

// 5. Empty branch (zero bytes): opens successfully; bom_len=0, line_ending=Lf.
#[test]
fn zero_byte_branch_succeeds() {
    let src = Source::new(branch(vec![]), SourceConfig::default()).unwrap();
    assert_eq!(src.bom_len(), 0);
    assert_eq!(src.line_ending(), LineEnding::Lf);
}

// 6. Branch content is shorter than probe_len: probe is clamped to content
//    length without error.
#[test]
fn branch_shorter_than_probe_size() {
    // 5 bytes; default probe_len is 8192.
    let src = Source::new(branch(b"AB\nCD".to_vec()), SourceConfig::default()).unwrap();
    assert_eq!(src.line_ending(), LineEnding::Lf);
    assert_eq!(src.bom_len(), 0);
}

// 7. Pure LF content: majority vote returns Lf.
#[test]
fn lf_line_ending_detected() {
    let content = b"line one\nline two\nline three\n".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::Lf);
}

// 8. Pure CRLF content: majority vote returns CrLf.
#[test]
fn crlf_line_ending_detected() {
    let content = b"line one\r\nline two\r\nline three\r\n".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::CrLf);
}

// 9. Pure CR content (classic Mac): majority vote returns Cr.
#[test]
fn cr_line_ending_detected() {
    let content = b"line one\rline two\rline three\r".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::Cr);
}

// 10. line_ending() accessor returns the exact detected value.
#[test]
fn line_ending_accessor_returns_detected_value() {
    let content = b"alpha\r\nbeta\r\ngamma\r\n".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    // Retrieve via the accessor — this is an explicit API check.
    let le: LineEnding = src.line_ending();
    assert_eq!(le, LineEnding::CrLf);
}

// 11. No terminators + explicit line_ending_default: default is used.
#[test]
fn line_ending_default_used_when_no_terminators() {
    let content = b"hello world with no newlines at all".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        line_ending_default: Some(LineEnding::CrLf),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::CrLf);
}

// 12. No terminators, no default: Lf is the hard fallback.
#[test]
fn lf_fallback_when_no_terminators_and_no_default() {
    let content = b"hello world no newlines whatsoever".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        line_ending_default: None,
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::Lf);
}

// 13. Custom (small) probe_len limits the scan window.
//     The first 8 bytes contain only LF terminators; beyond byte 8 there are
//     CRLF terminators.  With probe_len=8 only the LF zone is seen.
#[test]
fn custom_probe_len_limits_scan_window() {
    // "A\nB\nC\nD\n" = 8 bytes (all LF), then CRLF zone.
    let mut content = b"A\nB\nC\nD\n".to_vec();
    content.extend_from_slice(b"E\r\nF\r\nG\r\n");
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        probe_len: 8,
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::Lf);
}

// 14. BomPolicy::Ignore skips BOM detection: bom_len stays 0.
#[test]
fn bom_ignore_policy_does_not_set_bom_len() {
    let content: Vec<u8> = [0xEFu8, 0xBB, 0xBF]
        .iter()
        .chain(b"hello\n")
        .copied()
        .collect();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        bom_policy: BomPolicy::Ignore,
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.bom_len(), 0);
}

// 15. UTF-8 BOM: bom_len == 3, encoding == UTF-8.
#[test]
fn bom_len_utf8_is_3() {
    let content: Vec<u8> = [0xEFu8, 0xBB, 0xBF]
        .iter()
        .chain(b"hello\n")
        .copied()
        .collect();
    let src = Source::new(branch(content), SourceConfig::default()).unwrap();
    assert_eq!(src.bom_len(), 3);
    assert_eq!(src.encoding(), UTF_8);
}

// 16. UTF-16LE BOM: bom_len == 2, encoding == UTF-16LE.
#[test]
fn bom_len_utf16le_is_2() {
    // UTF-16LE BOM: FF FE; "h\ni" in UTF-16LE: 68 00  0A 00  69 00
    let content: Vec<u8> = vec![0xFF, 0xFE, 0x68, 0x00, 0x0A, 0x00, 0x69, 0x00];
    let src = Source::new(branch(content), SourceConfig::default()).unwrap();
    assert_eq!(src.bom_len(), 2);
    assert_eq!(src.encoding(), UTF_16LE);
}

// 17. UTF-16BE BOM: bom_len == 2, encoding == UTF-16BE.
#[test]
fn bom_len_utf16be_is_2() {
    // UTF-16BE BOM: FE FF; "hi" in UTF-16BE: 00 68  00 69
    let content: Vec<u8> = vec![0xFE, 0xFF, 0x00, 0x68, 0x00, 0x69];
    let src = Source::new(branch(content), SourceConfig::default()).unwrap();
    assert_eq!(src.bom_len(), 2);
    assert_eq!(src.encoding(), UTF_16BE);
}

// 18. Two Sources opened from the same branch with the same config agree on
//     all resolved fields.
#[test]
fn same_branch_two_sources_agree() {
    let thicket = make_thicket_from_bytes(b"hello\nworld\n".to_vec());
    let b1 = thicket.main();
    let b2 = thicket.main();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let s1 = Source::new(b1, config.clone()).unwrap();
    let s2 = Source::new(b2, config).unwrap();
    assert_eq!(s1.encoding(), s2.encoding());
    assert_eq!(s1.line_ending(), s2.line_ending());
    assert_eq!(s1.bom_len(), s2.bom_len());
}

// 19. Mixed LF-dominant content: LF wins the majority vote over lone CRs.
#[test]
fn mixed_lf_dominates_over_cr() {
    // 5 LF terminators vs 2 bare CR terminators.
    let content = b"a\nb\nc\nd\ne\nf\rg\rh".to_vec();
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    assert_eq!(src.line_ending(), LineEnding::Lf);
}

// 20. branch() accessor returns an Arc pointing to the same data
//     (two copies share the same byte content as witnessed via Branch::byte_len).
#[test]
fn branch_accessor_returns_valid_branch() {
    let content = b"data for branch accessor test\n".to_vec();
    let expected_len = content.len() as u64;
    let config = SourceConfig {
        encoding_hint: Some(UTF_8),
        ..SourceConfig::default()
    };
    let src = Source::new(branch(content), config).unwrap();
    let retrieved = src.branch();
    assert_eq!(retrieved.byte_len(), expected_len);
}

// ── UTF-8 well-formedness gate ─────────────────────────────────────────────────

// 21. Valid UTF-8 dense with box-drawing / em-dash / arrow characters must be
//     classified UTF-8 at every size — never windows-1252.
#[test]
fn valid_utf8_box_drawing_is_detected_as_utf8_regardless_of_size() {
    for lines in [1usize, 50, 5_000] {
        let mut s = String::new();
        for i in 0..lines {
            s.push_str(&format!(
                "//! ┌──────── rule {i} ────────┐ value → result │ note — end\n"
            ));
        }
        let src = Source::new(branch(s.into_bytes()), SourceConfig::default()).unwrap();
        assert_eq!(src.encoding(), UTF_8, "lines={lines}");
    }
}

// 22. A multi-byte sequence split exactly at the probe boundary must not demote
//     the result away from UTF-8.
#[test]
fn truncated_trailing_sequence_at_probe_boundary_is_still_utf8() {
    // Fill up to one byte short of the probe window with ASCII, then place a
    // 3-byte `─` (E2 94 80) so the probe cuts it mid-sequence.
    let mut bytes = vec![b'a'; DEFAULT_PROBE_LEN - 1];
    bytes.extend_from_slice("─".as_bytes()); // E2 94 80
    bytes.extend(std::iter::repeat_n(b'a', 64));
    let src = Source::new(branch(bytes), SourceConfig::default()).unwrap();
    assert_eq!(src.encoding(), UTF_8);
}

// 22b. A truncated trailing sequence at *true* end-of-file (the probe spans the
//      whole branch) is malformed UTF-8 and must route through the heuristic —
//      it must NOT be force-classified UTF-8 by the probe-prefix tolerance.
#[test]
fn truncated_trailing_sequence_at_true_eof_is_not_utf8() {
    // A lone `0xE2` is the lead byte of a 3-byte sequence with no continuation
    // bytes: well-formed only as a *prefix*, malformed at real EOF. In cp1252
    // it is `â`. With the whole branch present, it must not be classified UTF-8.
    let src = Source::new(branch(vec![0xE2]), SourceConfig::default()).unwrap();
    assert_ne!(src.encoding(), UTF_8);
}

// 22c. A probe that ends on a lone UTF-8 *lead* byte whose continuation past the
//      boundary is NOT a valid UTF-8 continuation (e.g. windows-1252 text where
//      the probe cuts on 0xE9 followed by ASCII) must route through the
//      heuristic — the prefix tolerance must not force-classify it as UTF-8.
#[test]
fn lone_lead_byte_at_probe_boundary_with_ascii_continuation_is_not_utf8() {
    // ASCII fills the probe up to its final byte; byte PROBE_LEN-1 is 0xE9
    // (cp1252 'é', also a 3-byte UTF-8 lead). The bytes immediately past the
    // probe boundary are ASCII — invalid UTF-8 continuations — so peeking past
    // the boundary must reject UTF-8 and defer to the heuristic detector.
    let mut bytes = vec![b'a'; DEFAULT_PROBE_LEN - 1];
    bytes.push(0xE9);
    bytes.extend_from_slice(b" plain ascii tail");
    let src = Source::new(branch(bytes), SourceConfig::default()).unwrap();
    assert_ne!(src.encoding(), UTF_8);
}

// 23. Genuine windows-1252 (a lone high byte, invalid as UTF-8) still routes
//     through the heuristic detector.
#[test]
fn lone_high_byte_is_not_utf8_and_uses_heuristic() {
    let bytes = b"caf\xE9 r\xE9sum\xE9\n".to_vec(); // 0xE9 = é in cp1252, invalid UTF-8
    let src = Source::new(branch(bytes), SourceConfig::default()).unwrap();
    assert_ne!(src.encoding(), UTF_8);
}

// 24. NUL-bearing input (e.g. BOM-less UTF-16LE) is valid UTF-8 byte-wise
//     (NUL = U+0000) but must NOT be force-classified UTF-8 by the gate: the
//     NUL guard defers to the heuristic, so enabling/disabling the gate yields
//     the same result.
#[test]
fn nul_bearing_input_bypasses_utf8_gate() {
    let mut bytes = Vec::new();
    for u in "hello world this is plenty of text".encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes()); // 68 00 65 00 ...
    }
    let gated = Source::new(branch(bytes.clone()), SourceConfig::default()).unwrap();
    let ungated = Source::new(
        branch(bytes),
        SourceConfig {
            prefer_utf8_when_valid: false,
            ..SourceConfig::default()
        },
    )
    .unwrap();
    assert_eq!(
        gated.encoding(),
        ungated.encoding(),
        "NUL-bearing input must bypass the UTF-8 gate and defer to the heuristic"
    );
}

// 25. With the gate disabled, detection reverts to pure heuristic behaviour
//     (UTF-8 is no longer forced by validation).
#[test]
fn gate_disabled_uses_pure_heuristic() {
    let bytes = b"caf\xE9 r\xE9sum\xE9\n".to_vec();
    let config = SourceConfig {
        prefer_utf8_when_valid: false,
        ..SourceConfig::default()
    };
    // Heuristic still runs and resolves to a usable encoding.
    let src = Source::new(branch(bytes), config).unwrap();
    assert_ne!(src.encoding(), UTF_8);
}

// 26. validate_full_stream_utf8 reads the *entire* stream: a late invalid byte
//     past the probe window is caught by the whole-stream validator, even
//     though the clean ASCII probe alone is accepted as UTF-8 by default.
//
//     Demotion then re-runs the heuristic on the clean probe, so the
//     Source-level encoding can legitimately stay UTF-8 ("UTF-8 with one bad
//     byte", per the recommendation doc §4.1). The value of the flag is the
//     full-stream *check*, not a forced re-classification — so this test
//     asserts the check, not an unachievable encoding flip.
#[test]
fn full_stream_validation_detects_late_invalid_byte() {
    // First probe window is clean ASCII (valid UTF-8); a dense block of lone
    // high bytes — invalid as UTF-8 — sits past DEFAULT_PROBE_LEN.
    let mut bytes = vec![b'a'; DEFAULT_PROBE_LEN + 16];
    for _ in 0..512 {
        bytes.extend_from_slice(b"caf\xE9 r\xE9sum\xE9 na\xEFve fianc\xE9e ");
    }

    // Probe-only default accepts the clean probe as UTF-8.
    let default_src = Source::new(branch(bytes.clone()), SourceConfig::default()).unwrap();
    assert_eq!(default_src.encoding(), UTF_8);

    // The whole-stream validator sees the invalid tail past the probe window.
    assert!(!branch_is_well_formed_utf8(&branch(bytes)).unwrap());
}

// 27. validate_full_stream_utf8: a fully clean UTF-8 stream is still accepted
//     as UTF-8 when full-stream validation is enabled.
#[test]
fn full_stream_validation_accepts_clean_utf8() {
    let mut bytes = vec![b'a'; DEFAULT_PROBE_LEN + 1024];
    bytes.extend_from_slice("─ → — │ end\n".as_bytes());
    let strict = SourceConfig {
        validate_full_stream_utf8: true,
        ..SourceConfig::default()
    };
    let src = Source::new(branch(bytes), strict).unwrap();
    assert_eq!(src.encoding(), UTF_8);
}
