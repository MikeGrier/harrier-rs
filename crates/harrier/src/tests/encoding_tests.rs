// Copyright (c) 2026, Michael Grier

//! Unit tests for `crate::encoding` (MA-8, MA-9).

use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE};

use crate::encoding::{
    BomPolicy, DEFAULT_PROBE_LEN, DecodeErrorPolicy, LineEnding, SourceConfig, detect_bom,
    detect_line_ending,
};

// MA-9 uses the trait object separately so the import is scoped here.
#[cfg(test)]
mod trait_object_tests {
    use encoding_rs::{UTF_8, WINDOWS_1252};

    use crate::encoding::{ChardetngDetector, EncodingDetector};

    #[test]
    fn chardetng_detector_boxed_as_trait_object_guesses_utf8() {
        let mut det: Box<dyn EncodingDetector> = Box::new(ChardetngDetector::new());
        // Plain ASCII is valid UTF-8; chardetng with allow_utf8=true should return UTF-8.
        det.feed(b"Hello, world! This is ASCII text.", false);
        det.feed(b"More plain text to give the detector signal.", true);
        let enc = det.guess(true);
        assert_eq!(enc, UTF_8);
    }

    #[test]
    fn chardetng_detector_default_same_as_new() {
        // `Default` delegates to `new`; both should produce equivalent results
        // when fed and queried identically.
        let mut a: Box<dyn EncodingDetector> = Box::new(ChardetngDetector::new());
        let mut b: Box<dyn EncodingDetector> = Box::new(ChardetngDetector::default());
        let text = b"Hello world, plain ASCII.";
        a.feed(text, true);
        b.feed(text, true);
        assert_eq!(a.guess(true), b.guess(true));
    }

    #[test]
    fn chardetng_detector_allow_utf8_false_does_not_return_utf8() {
        let mut det: Box<dyn EncodingDetector> = Box::new(ChardetngDetector::new());
        // Feed valid UTF-8; with allow_utf8=false the result must not be UTF-8.
        det.feed(b"Hello, world! Plain ASCII.", true);
        let enc = det.guess(false);
        assert_ne!(enc, UTF_8);
    }

    #[test]
    fn chardetng_detector_guess_called_before_feed_does_not_panic() {
        // Calling guess on an unfed detector should not panic.
        let det = ChardetngDetector::new();
        let _enc = det.guess(true);
    }

    #[test]
    fn chardetng_detector_last_true_signals_end_of_stream() {
        // After signalling last=true the detector should still return a result.
        let mut det: Box<dyn EncodingDetector> = Box::new(ChardetngDetector::new());
        det.feed(b"Some text.", true);
        let enc = det.guess(true);
        // Must be a valid (non-null) encoding pointer — encoding_rs guarantees this.
        assert!(!enc.name().is_empty());
    }

    #[test]
    fn chardetng_detector_windows1252_latin_content() {
        // Windows-1252 bytes for "café résumé naïve" (accented Latin text).
        // chardetng should guess a Western European encoding for dense Latin-1
        // content when UTF-8 is disallowed.
        let latin_bytes: Vec<u8> = vec![
            // "café résumé" repeated to give the detector enough signal.
            b'c', b'a', b'f', 0xE9, b' ', 0xE9, b'l', 0xE8, b'v', 0xE9, b' ', b'n', b'a', 0xEF,
            b'v', b'e', b' ', b'r', 0xE9, b's', b'u', b'm', 0xE9, b' ', b'c', b'a', b'f', 0xE9,
            b' ', 0xE9, b'l', 0xE8, b'v', 0xE9, b' ', b'n', b'a', 0xEF, b'v', b'e', b' ', b'r',
            0xE9, b's', b'u', b'm', 0xE9, b' ', b'c', b'a', b'f', 0xE9,
        ];
        let mut det: Box<dyn EncodingDetector> = Box::new(ChardetngDetector::new());
        det.feed(&latin_bytes, true);
        // Ask without allowing UTF-8 since these bytes are not valid UTF-8.
        let enc = det.guess(false);
        // The result should be a single-byte Western European encoding.
        // We accept windows-1252 or iso-8859-1 (which encoding_rs unifies as windows-1252).
        assert_eq!(
            enc,
            WINDOWS_1252,
            "expected a Western European encoding, got {}",
            enc.name()
        );
    }
}

// ── MA-8: Phase 1 unit tests ──────────────────────────────────────────────────

// ── BOM detection ─────────────────────────────────────────────────────────────

#[test]
fn bom_utf8_detected() {
    let result = detect_bom(&[0xEF, 0xBB, 0xBF, b'h', b'i']);
    assert_eq!(result.encoding, Some(UTF_8));
    assert_eq!(result.bom_len, 3);
}

#[test]
fn bom_utf16le_detected() {
    let result = detect_bom(&[0xFF, 0xFE, 0x00, 0x00]);
    assert_eq!(result.encoding, Some(UTF_16LE));
    assert_eq!(result.bom_len, 2);
}

#[test]
fn bom_utf16be_detected() {
    let result = detect_bom(&[0xFE, 0xFF, 0x00, 0x00]);
    assert_eq!(result.encoding, Some(UTF_16BE));
    assert_eq!(result.bom_len, 2);
}

#[test]
fn bom_none_when_no_bom_present() {
    let result = detect_bom(b"hello world");
    assert_eq!(result.encoding, None);
    assert_eq!(result.bom_len, 0);
}

#[test]
fn bom_none_for_probe_shorter_than_bom() {
    // Two bytes is not enough for any recognised BOM (UTF-8 needs 3,
    // UTF-16 needs 2 — but 0xFE alone is not a valid UTF-8 BOM).
    let result = detect_bom(&[0xEF, 0xBB]);
    assert_eq!(result.encoding, None);
    assert_eq!(result.bom_len, 0);
}

#[test]
fn bom_none_for_empty_probe() {
    let result = detect_bom(&[]);
    assert_eq!(result.encoding, None);
    assert_eq!(result.bom_len, 0);
}

// ── Line-ending majority vote ─────────────────────────────────────────────────

#[test]
fn line_ending_pure_lf() {
    let probe = b"alpha\nbeta\ngamma\n";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::Lf));
}

#[test]
fn line_ending_pure_crlf() {
    let probe = b"alpha\r\nbeta\r\ngamma\r\n";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::CrLf));
}

#[test]
fn line_ending_pure_cr() {
    let probe = b"alpha\rbeta\rgamma\r";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::Cr));
}

#[test]
fn line_ending_lf_majority_over_crlf() {
    // 3 LF vs 1 CRLF — LF wins outright.
    let probe = b"a\nb\nc\nd\r\ne";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::Lf));
}

#[test]
fn line_ending_crlf_not_double_counted_as_cr() {
    // 2 CRLF, 0 bare CR — the \r in each CRLF must not also increment CR.
    let probe = b"a\r\nb\r\n";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::CrLf));
}

#[test]
fn line_ending_tie_resolved_by_caller_default() {
    // 2 LF, 2 CRLF — tie; caller prefers CRLF.
    let probe = b"a\nb\nc\r\nd\r\n";
    assert_eq!(
        detect_line_ending(probe, Some(LineEnding::CrLf)),
        Some(LineEnding::CrLf)
    );
}

#[test]
fn line_ending_tie_caller_default_not_among_leaders_falls_to_first_appearance() {
    // 2 LF, 2 CRLF — tie; caller default is CR (not a leader) → first
    // appearance wins.  LF appears before CRLF in this probe.
    let probe = b"a\nb\r\nc\nd\r\n";
    assert_eq!(
        detect_line_ending(probe, Some(LineEnding::Cr)),
        Some(LineEnding::Lf)
    );
}

#[test]
fn line_ending_tie_no_default_resolved_by_first_appearance() {
    // 1 CRLF, 1 LF — tie; no caller default.  CRLF appears first.
    let probe = b"first\r\nsecond\n";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::CrLf));
}

#[test]
fn line_ending_tie_no_default_lf_first() {
    // 1 LF, 1 CRLF — tie; no caller default.  LF appears first.
    let probe = b"first\nsecond\r\n";
    assert_eq!(detect_line_ending(probe, None), Some(LineEnding::Lf));
}

#[test]
fn line_ending_none_when_no_terminators() {
    let probe = b"no newlines here at all";
    assert_eq!(detect_line_ending(probe, None), None);
}

#[test]
fn line_ending_none_for_empty_probe() {
    assert_eq!(detect_line_ending(&[], None), None);
}

// ── DecodeErrorPolicy and SourceConfig construction ───────────────────────────

#[test]
fn decode_error_policy_validate_first_exists() {
    // Compile-time check that the variant can be named and compared.
    let policy = DecodeErrorPolicy::ValidateFirst;
    assert_eq!(policy, DecodeErrorPolicy::ValidateFirst);
}

#[test]
fn source_config_default_values() {
    let cfg = SourceConfig::default();
    assert_eq!(cfg.encoding_hint, None);
    assert_eq!(cfg.bom_policy, BomPolicy::Honour);
    assert_eq!(cfg.decode_error_policy, DecodeErrorPolicy::Substitute);
    assert_eq!(cfg.line_ending_default, None);
    assert_eq!(cfg.probe_len, DEFAULT_PROBE_LEN);
    assert!(cfg.prefer_utf8_when_valid);
    assert!(!cfg.validate_full_stream_utf8);
}

#[test]
fn source_config_custom_values_round_trip() {
    let cfg = SourceConfig {
        encoding_hint: Some(UTF_8),
        prefer_utf8_when_valid: false,
        validate_full_stream_utf8: true,
        bom_policy: BomPolicy::Ignore,
        decode_error_policy: DecodeErrorPolicy::Fatal,
        line_ending_default: Some(LineEnding::CrLf),
        probe_len: 512,
    };
    assert_eq!(cfg.encoding_hint, Some(UTF_8));
    assert_eq!(cfg.bom_policy, BomPolicy::Ignore);
    assert_eq!(cfg.decode_error_policy, DecodeErrorPolicy::Fatal);
    assert_eq!(cfg.line_ending_default, Some(LineEnding::CrLf));
    assert_eq!(cfg.probe_len, 512);
    assert!(!cfg.prefer_utf8_when_valid);
    assert!(cfg.validate_full_stream_utf8);
}
