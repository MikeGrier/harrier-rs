// Copyright (c) 2026, Michael Grier

//! ML-54: Unit tests for `weaver::mallard_bridge`.
//!
//! Covers:
//!   - `from_grouse`: line count, content, terminator stripping, multi-byte
//!     encoding decoding, optional extra validator, empty and single-line edge cases.
//!   - `GrouseEncodingValidator`: ASCII fast-path, UTF-8 acceptance, rejection
//!     of non-Latin-1 content under Windows-1252, acceptance of Latin-1 content,
//!     empty-string acceptance.
//!   - `GrouseLineSource`: line-count hint and ordering.
//!   - Buffer-level validation: inserting lines into a buffer built from a
//!     Windows-1252 source enforces the encoding constraint at edit time.

use std::sync::Arc;

use encoding_rs::{UTF_8, WINDOWS_1252};
use mallard::{EncodingValidator, LineSource};
use redwing::make_thicket_from_bytes;

use crate::{
    encoding::{BomPolicy, SourceConfig},
    lines::Lines,
    mallard_bridge::{from_grouse, GrouseEncodingValidator, GrouseLineSource},
    source::Source,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn make_source(bytes: impl Into<Vec<u8>>, encoding: &'static encoding_rs::Encoding) -> Source {
    let branch = make_thicket_from_bytes(bytes.into()).main();
    Source::new(
        branch,
        SourceConfig {
            encoding_hint: Some(encoding),
            bom_policy: BomPolicy::Ignore,
            ..SourceConfig::default()
        },
    )
    .unwrap()
}

fn make_lines(bytes: impl Into<Vec<u8>>, encoding: &'static encoding_rs::Encoding) -> Lines {
    make_source(bytes, encoding).as_lines().unwrap()
}

// ── from_grouse: line count and content ───────────────────────────────────────

/// 1. Three LF-terminated UTF-8 lines: count and content are correct.
#[test]
fn from_grouse_utf8_lf_three_lines() {
    let src = make_source(b"alpha\nbeta\ngamma\n", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 3);
    assert_eq!(lb.get_line(0).as_deref(), Some("alpha"));
    assert_eq!(lb.get_line(1).as_deref(), Some("beta"));
    assert_eq!(lb.get_line(2).as_deref(), Some("gamma"));
}

/// 2. Three CRLF-terminated UTF-8 lines: `\r` is stripped alongside `\n`.
#[test]
fn from_grouse_utf8_crlf_three_lines() {
    let src = make_source(b"one\r\ntwo\r\nthree\r\n", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 3);
    assert_eq!(lb.get_line(0).as_deref(), Some("one"));
    assert_eq!(lb.get_line(1).as_deref(), Some("two"));
    assert_eq!(lb.get_line(2).as_deref(), Some("three"));
}

/// 3. Empty source (zero bytes) → LineBuffer with zero lines.
#[test]
fn from_grouse_empty_source() {
    let src = make_source(b"", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 0);
    assert!(lb.get_line(0).is_none());
}

/// 4. Single line with no trailing newline → one line, content intact.
#[test]
fn from_grouse_single_line_no_newline() {
    let src = make_source(b"hello world", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 1);
    assert_eq!(lb.get_line(0).as_deref(), Some("hello world"));
}

/// 5. Single line with trailing LF: one line, LF stripped.
#[test]
fn from_grouse_single_line_with_lf() {
    let src = make_source(b"hello\n", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 1);
    assert_eq!(lb.get_line(0).as_deref(), Some("hello"));
}

/// 6. Multi-byte UTF-8 characters (Japanese) pass through intact.
#[test]
fn from_grouse_utf8_multibyte_characters() {
    // "東京\n大阪\n" — Japanese, entirely multi-byte UTF-8.
    let src = make_source("東京\n大阪\n".as_bytes(), UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 2);
    assert_eq!(lb.get_line(0).as_deref(), Some("東京"));
    assert_eq!(lb.get_line(1).as_deref(), Some("大阪"));
}

/// 7. Windows-1252 bytes are decoded to the correct UTF-8 by `from_grouse`.
///
/// 0xE9 = 'é' in Windows-1252; the resulting LineBuffer line must be "café".
#[test]
fn from_grouse_windows_1252_decoded_to_utf8() {
    // "caf\xE9\n" in Windows-1252 → "café"
    let src = make_source(b"caf\xe9\n", WINDOWS_1252);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 1);
    assert_eq!(lb.get_line(0).as_deref(), Some("café"));
}

/// 8. Five blank lines (just LFs): count and empty content.
#[test]
fn from_grouse_all_blank_lines() {
    let src = make_source(b"\n\n\n\n\n", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 5);
    for i in 0..5 {
        assert_eq!(
            lb.get_line(i).as_deref(),
            Some(""),
            "line {i} should be empty"
        );
    }
}

/// 9. Line at boundary of out-of-range access returns None.
#[test]
fn from_grouse_out_of_range_returns_none() {
    let src = make_source(b"only\n", UTF_8);
    let lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 1);
    assert!(lb.get_line(1).is_none());
}

/// 10. `from_grouse` with an extra validator that accepts: no error, line present.
#[test]
fn from_grouse_with_accepting_extra_validator() {
    use mallard::EncodingError;

    struct AlwaysOk;
    impl EncodingValidator for AlwaysOk {
        fn validate(&self, _line: &str) -> Result<(), EncodingError> {
            Ok(())
        }
    }

    let src = make_source(b"line1\nline2\n", UTF_8);
    let lb = from_grouse(src, Some(Arc::new(AlwaysOk))).unwrap();
    assert_eq!(lb.line_count(), 2);
    assert_eq!(lb.get_line(0).as_deref(), Some("line1"));
}

/// 11. `from_grouse` with an extra validator that rejects: returns Err.
#[test]
fn from_grouse_with_rejecting_extra_validator() {
    use mallard::EncodingError;

    struct AlwaysReject;
    impl EncodingValidator for AlwaysReject {
        fn validate(&self, line: &str) -> Result<(), EncodingError> {
            Err(EncodingError {
                line: line.into(),
                description: "test rejection".into(),
            })
        }
    }

    let src = make_source(b"blocked\n", UTF_8);
    let result = from_grouse(src, Some(Arc::new(AlwaysReject)));
    assert!(
        result.is_err(),
        "extra validator should have caused rejection"
    );
}

// ── GrouseEncodingValidator ───────────────────────────────────────────────────

/// 12. Validator accepts pure ASCII with any encoding.
#[test]
fn validator_accepts_pure_ascii() {
    let v = GrouseEncodingValidator::new(WINDOWS_1252);
    assert!(v.validate("Hello, world!").is_ok());
    assert!(v.validate("").is_ok());
    assert!(v.validate("abcdefghijklmnopqrstuvwxyz 0123456789").is_ok());
}

/// 13. Validator with UTF-8 encoding accepts any valid UTF-8 content.
#[test]
fn validator_utf8_accepts_unicode() {
    let v = GrouseEncodingValidator::new(UTF_8);
    assert!(v.validate("Hello, 世界!").is_ok());
    assert!(v.validate("Привет мир").is_ok());
    assert!(v.validate("😀🎉").is_ok());
}

/// 14. Validator with Windows-1252 accepts Latin-1-encodable characters.
#[test]
fn validator_windows_1252_accepts_latin1() {
    let v = GrouseEncodingValidator::new(WINDOWS_1252);
    // All of these are representable in Windows-1252.
    assert!(v.validate("café").is_ok());
    assert!(v.validate("naïve").is_ok());
    assert!(v.validate("résumé").is_ok());
    assert!(v.validate("Ångström").is_ok());
}

/// 15. Validator with Windows-1252 rejects characters outside its range.
#[test]
fn validator_windows_1252_rejects_non_latin1() {
    let v = GrouseEncodingValidator::new(WINDOWS_1252);
    // Japanese, Chinese, emoji — none are representable in Windows-1252.
    assert!(v.validate("東京").is_err(), "Japanese should be rejected");
    assert!(v.validate("Привет").is_err(), "Cyrillic should be rejected");
    assert!(v.validate("😀").is_err(), "emoji should be rejected");
}

/// 16. Error message from validator contains the encoding name.
#[test]
fn validator_error_contains_encoding_name() {
    let v = GrouseEncodingValidator::new(WINDOWS_1252);
    let err = v.validate("東京").unwrap_err();
    let desc = err.description.as_ref();
    assert!(
        desc.contains("windows-1252") || desc.contains("Windows-1252"),
        "error description should mention the encoding: {desc:?}"
    );
}

// ── GrouseLineSource ──────────────────────────────────────────────────────────

/// 17. `line_count_hint` matches actual line count.
#[test]
fn line_source_count_hint_matches() {
    let lines = make_lines(b"a\nb\nc\n", UTF_8);
    let src = GrouseLineSource::new(lines);
    let hint = src.line_count_hint();
    assert_eq!(hint, Some(3));
    let mut count = 0;
    src.for_each_line(&mut |_| count += 1);
    assert_eq!(count, 3);
}

/// 18. `for_each_line` delivers lines in order.
#[test]
fn line_source_order_preserved() {
    let lines = make_lines(b"first\nsecond\nthird\n", UTF_8);
    let src = GrouseLineSource::new(lines);
    let mut collected: Vec<String> = Vec::new();
    src.for_each_line(&mut |l| collected.push(l.to_string()));
    assert_eq!(collected, ["first", "second", "third"]);
}

// ── Buffer-level validation at edit time ──────────────────────────────────────

/// 19. A LineBuffer built from a Windows-1252 source rejects non-Latin-1
///     content at `insert_line` time (GrouseEncodingValidator is installed).
#[test]
fn edit_windows_1252_buffer_rejects_non_latin1_insert() {
    // Build a source with valid Windows-1252 text, load it via from_grouse.
    let src = make_source(b"hello\n", WINDOWS_1252);
    let mut lb = from_grouse(src, None).unwrap();
    assert_eq!(lb.line_count(), 1);

    // Inserting a Latin-1-safe string succeeds.
    assert!(lb.insert_line(1, "café").is_ok());

    // Inserting Japanese (outside Windows-1252) must fail.
    let result = lb.insert_line(2, "東京");
    assert!(
        result.is_err(),
        "non-Latin-1 insert into win-1252 buffer must fail"
    );
}

/// 20. A LineBuffer built from a UTF-8 source accepts any Unicode at edit time.
#[test]
fn edit_utf8_buffer_accepts_any_unicode_insert() {
    let src = make_source(b"start\n", UTF_8);
    let mut lb = from_grouse(src, None).unwrap();

    assert!(lb.insert_line(1, "日本語").is_ok());
    assert!(lb.insert_line(2, "emoji: 🦆").is_ok());
    assert!(lb.insert_line(3, "Привет").is_ok());
    assert_eq!(lb.line_count(), 4);
}
