// Copyright (c) 2026, Michael Grier

//! MA-13: Unit tests for `Source::new`.

use std::sync::Arc;

use encoding_rs::{UTF_16BE, UTF_16LE, UTF_8, WINDOWS_1252};
use redwing::{make_thicket_from_bytes, Branch};

use crate::{
    encoding::{BomPolicy, LineEnding, SourceConfig},
    source::{Source, SourceError},
};

// ── Helper ────────────────────────────────────────────────────────────────────

fn branch(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

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
