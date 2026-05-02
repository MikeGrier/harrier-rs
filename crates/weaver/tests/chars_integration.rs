// Copyright (c) 2026, Michael Grier

//! MA-IT-1: Integration tests for `Chars`.
//!
//! Opens UTF-8, UTF-16LE, and a DBCS-encoded (Shift_JIS) byte stream; iterates
//! all characters via `chars_from`; verifies that every character-start offset
//! returns `true` from `is_boundary`; and verifies `encode` round-trips for
//! ASCII and non-ASCII content.
//!
//! All test data is generated at runtime from known Unicode strings so that
//! there are no external file dependencies.

use std::sync::Arc;

use encoding_rs::{UTF_16LE, UTF_8};
use weaver::{
    encoding::{BomPolicy, SourceConfig},
    source::Source,
};
use redwing::{make_thicket_from_bytes, Branch};

// ── Helper ────────────────────────────────────────────────────────────────────

fn branch(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

fn make_source(bytes: Vec<u8>, enc: &'static encoding_rs::Encoding) -> weaver::source::Source {
    let config = SourceConfig {
        encoding_hint: Some(enc),
        bom_policy: BomPolicy::Ignore,
        ..SourceConfig::default()
    };
    Source::new(branch(bytes), config).expect("Source::new")
}

// ── MA-IT-1a: UTF-8 ──────────────────────────────────────────────────────────

/// Verify that chars_from(0) over a UTF-8 branch yields exactly the characters
/// in the source string, and that every character-start byte offset is a boundary.
#[test]
fn utf8_chars_from_round_trip() {
    // Multi-line, multi-byte ASCII+Unicode content.
    let expected = "The quick brown fox jumps over the lazy dog.\n\
                    Voilà une ligne avec des caractères accentués: é à ü ö.\n\
                    CJK: 世界 你好\n\
                    Emoji: 🦊 🐶\n\
                    End of test.\n";

    let src = make_source(expected.as_bytes().to_vec(), UTF_8);
    let chars = src.as_chars().expect("as_chars");

    // Collect all characters from the start.
    let collected: String = chars.chars_from(0).collect();
    assert_eq!(
        collected, expected,
        "chars_from must yield the full source string"
    );

    // Verify is_boundary at every byte offset in the UTF-8 source.
    // For each byte, it is a boundary iff it is NOT a UTF-8 continuation byte.
    for (byte_offset, &byte) in expected.as_bytes().iter().enumerate() {
        let is_cont = (byte & 0xC0) == 0x80;
        let got = chars.is_boundary(byte_offset as u64).expect("is_boundary");
        assert_eq!(
            got, !is_cont,
            "offset {byte_offset}: byte 0x{byte:02X} expected boundary={}, got={got}",
            !is_cont
        );
    }
}

/// Verify that chars_from at the start of each character (non-zero offsets)
/// yields the remainder of the string correctly.
#[test]
fn utf8_chars_from_at_each_boundary() {
    let input = "ABé世😀XY";
    let src = make_source(input.as_bytes().to_vec(), UTF_8);
    let chars = src.as_chars().expect("as_chars");

    // For each character boundary, check chars_from returns the correct suffix.
    let mut byte_offset: u64 = 0;
    for ch in input.chars() {
        let suffix = &input[byte_offset as usize..];
        let collected: String = chars.chars_from(byte_offset).collect();
        assert_eq!(
            collected, suffix,
            "chars_from({byte_offset}) should yield \"{suffix}\""
        );
        byte_offset += ch.len_utf8() as u64;
    }
}

/// encode round-trip in UTF-8: encode a string containing ASCII and multi-byte
/// characters; decoding the bytes back must give the original string.
#[test]
fn utf8_encode_round_trip() {
    use weaver::encoded::Encoded;
    let input = "Hello, 世界! é à ü 🦊";
    let src = make_source(b"placeholder".to_vec(), UTF_8);
    let chars = src.as_chars().expect("as_chars");

    let bytes = chars.encode(input).expect("encode");
    let (decoded, _enc, had_errors) = UTF_8.decode(&bytes);
    assert!(!had_errors);
    assert_eq!(decoded.as_ref(), input);
}

// ── MA-IT-1b: UTF-16LE ───────────────────────────────────────────────────────

/// Encode `s` as raw UTF-16LE bytes (no BOM).
///
/// Uses Rust's built-in `encode_utf16()` iterator rather than
/// `encoding_rs::UTF_16LE.encode()`, which re-routes through
/// `output_encoding()` (UTF-8) and would produce UTF-8 bytes instead.
fn to_utf16le(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}

/// Verify chars_from over a UTF-16LE branch yields the correct characters and
/// that every code-unit boundary is correctly classified.
#[test]
fn utf16le_chars_from_round_trip() {
    let expected = "Hello, World!\nLine two.\nThe end.\n";
    let raw = to_utf16le(expected);
    let raw_len = raw.len() as u64;

    let src = make_source(raw, UTF_16LE);
    let chars = src.as_chars().expect("as_chars");

    let collected: String = chars.chars_from(0).collect();
    assert_eq!(collected, expected);

    // For ASCII-only UTF-16LE content, every even offset is a boundary and
    // every odd offset is not.
    for offset in 0..raw_len {
        let expected_boundary = offset % 2 == 0;
        let got = chars.is_boundary(offset).expect("is_boundary");
        assert_eq!(
            got, expected_boundary,
            "UTF-16LE offset {offset}: expected boundary={expected_boundary}"
        );
    }
}

/// Verify that a UTF-16LE stream containing a surrogate pair (supplementary
/// character) is decoded correctly, with the high-surrogate offset as the
/// boundary and the low-surrogate offset NOT as a boundary.
#[test]
fn utf16le_surrogate_pair_chars_from() {
    // '😀' U+1F600 encoded as UTF-16LE:
    //   High surrogate U+D83D → [0x3D, 0xD8]
    //   Low  surrogate U+DE00 → [0x00, 0xDE]
    // Preceded by 'A' = [0x41, 0x00].
    let content = vec![0x41, 0x00, 0x3D, 0xD8, 0x00, 0xDE];
    let src = make_source(content, UTF_16LE);
    let chars = src.as_chars().expect("as_chars");

    // Chars should be: 'A', '😀'
    let collected: String = chars.chars_from(0).collect();
    assert_eq!(collected, "A😀");

    // High-surrogate word (offset 2) is a boundary; low-surrogate (offset 4) is not.
    assert!(chars.is_boundary(2).expect("boundary at high surrogate"));
    assert!(!chars.is_boundary(4).expect("boundary at low surrogate"));
}

/// encode() for a UTF-16LE source verifies that the output round-trips.
///
/// # encoding_rs behaviour note
///
/// `encoding_rs::UTF_16LE` is decode-only in the WHATWG Encoding Standard.
/// When `Encoded::encode` calls `UTF_16LE.new_encoder()`, encoding_rs returns
/// a UTF-8 encoder rather than a UTF-16LE encoder.  The resulting byte
/// sequence is therefore UTF-8, not UTF-16LE.  This test verifies that exact
/// behaviour: `chars.encode(input)` returns valid UTF-8 bytes that decode back
/// to the original string.
#[test]
fn utf16le_encode_round_trip() {
    use weaver::encoded::Encoded;
    let input = "Hello, World!";
    let src = make_source(to_utf16le(input), UTF_16LE);
    let chars = src.as_chars().expect("as_chars");

    // encode() for a UTF-16LE source produces UTF-8 output (encoding_rs
    // silently falls back to UTF-8 for decode-only WHATWG encodings).
    let encoded = chars.encode(input).expect("encode");
    let (decoded, _, had_errors) = encoding_rs::UTF_8.decode(&encoded);
    assert!(!had_errors, "encoded bytes must be valid UTF-8");
    assert_eq!(
        decoded.as_ref(),
        input,
        "UTF-8 round-trip must preserve input"
    );
}

// ── MA-IT-1c: DBCS (Shift_JIS) ───────────────────────────────────────────────

/// Helper: encode a &str as Shift_JIS bytes.  Only works for characters that
/// have a Shift_JIS representation; panics if encoding produces errors.
fn to_shift_jis(s: &str) -> Vec<u8> {
    let (bytes, _enc, had_errors) = encoding_rs::SHIFT_JIS.encode(s);
    assert!(
        !had_errors,
        "Shift_JIS encoding of test string must not error"
    );
    bytes.into_owned()
}

/// Verify that chars_from starting at a known Shift_JIS anchor (LF) yields
/// the correct tail of the stream.
#[test]
fn shift_jis_chars_from_at_anchor() {
    // Content: 'あ' (0x82A0 in Shift_JIS, a 2-byte sequence), then LF, then "World"
    let mut content = to_shift_jis("あ");
    content.push(0x0A); // LF
    content.extend_from_slice(b"World");

    let lf_offset = to_shift_jis("あ").len() as u64; // 2

    let src = make_source(content, encoding_rs::SHIFT_JIS);
    let chars = src.as_chars().expect("as_chars");

    // chars_from at the LF (a known anchor) must give "\nWorld".
    let collected: String = chars.chars_from(lf_offset).collect();
    assert_eq!(collected, "\nWorld");
}

/// Verify is_boundary at Shift_JIS anchor bytes.
#[test]
fn shift_jis_is_boundary_at_anchors() {
    // Content: 'あ' (2 bytes), LF, 'い' (2 bytes), CR, 'う' (2 bytes)
    let mut content = to_shift_jis("あ");
    content.push(0x0A); // LF at offset 2
    content.extend_from_slice(&to_shift_jis("い"));
    content.push(0x0D); // CR at offset 7
    content.extend_from_slice(&to_shift_jis("う"));

    let src = make_source(content, encoding_rs::SHIFT_JIS);
    let chars = src.as_chars().expect("as_chars");

    // Content layout: あ(0,1), LF(2), い(3,4), CR(5), う(6,7)
    assert!(
        chars.is_boundary(2).expect("LF is boundary"),
        "LF at offset 2"
    );
    assert!(
        chars.is_boundary(5).expect("CR is boundary"),
        "CR at offset 5"
    );
    // Trail bytes are not boundaries.
    assert!(
        !chars.is_boundary(1).expect("trail byte 1"),
        "trail of 'あ'"
    );
    assert!(
        !chars.is_boundary(4).expect("trail byte 4"),
        "trail of 'い'"
    );
}

/// Verify that chars_from iterates through a longer, multi-line Shift_JIS
/// stream and produces the same content as decoding the whole buffer.
#[test]
fn shift_jis_full_iteration_matches_expected() {
    // Build a multi-line Shift_JIS stream.
    let lines = ["line one", "line two", "line three", "final line"];
    let expected = lines.join("\n") + "\n";

    // ASCII content: Shift_JIS encodes ASCII identically to UTF-8.
    let content = to_shift_jis(&expected);

    let src = make_source(content, encoding_rs::SHIFT_JIS);
    let chars = src.as_chars().expect("as_chars");

    let collected: String = chars.chars_from(0).collect();
    assert_eq!(collected, expected);
}

/// encode round-trip in Shift_JIS: ASCII content stays ASCII.
#[test]
fn shift_jis_encode_round_trip_ascii() {
    use weaver::encoded::Encoded;
    let input = "Hello, Shift_JIS!\n";
    let src = make_source(to_shift_jis(input), encoding_rs::SHIFT_JIS);
    let chars = src.as_chars().expect("as_chars");

    let bytes = chars.encode(input).expect("encode");
    let (decoded, _enc, had_errors) = encoding_rs::SHIFT_JIS.decode(&bytes);
    assert!(!had_errors);
    assert_eq!(decoded.as_ref(), input);
}
