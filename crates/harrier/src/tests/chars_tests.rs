// Copyright (c) 2026, Michael Grier

//! MA-22: Unit tests for `Chars` — is_boundary, nearest_sync_point, chars_from,
//! and encode round-trips.

use std::sync::Arc;

use encoding_rs::{UTF_8, UTF_16BE, UTF_16LE, WINDOWS_1252};
use redwing::{Branch, make_thicket_from_bytes};

use crate::{
    chars::Chars,
    encoded::Encoded,
    encoding::{BomPolicy, SourceConfig},
    source::Source,
};

// ── Helper ────────────────────────────────────────────────────────────────────

fn branch(bytes: impl Into<Vec<u8>>) -> Arc<dyn Branch> {
    make_thicket_from_bytes(bytes.into()).main()
}

/// Build a `Chars` from raw bytes with an explicit encoding hint and no BOM.
fn chars_with_encoding(bytes: Vec<u8>, enc: &'static encoding_rs::Encoding) -> Chars {
    let config = SourceConfig {
        encoding_hint: Some(enc),
        bom_policy: BomPolicy::Ignore,
        ..SourceConfig::default()
    };
    Source::new(branch(bytes), config)
        .expect("Source::new")
        .as_chars()
        .expect("as_chars")
}

// ── UTF-8 is_boundary tests ───────────────────────────────────────────────────

// 1. An ASCII byte (0xxxxxxx) is always a UTF-8 character boundary.
#[test]
fn utf8_ascii_byte_is_boundary() {
    let chars = chars_with_encoding(b"A".to_vec(), UTF_8);
    assert!(chars.is_boundary(0).unwrap());
}

// 2. The lead byte of a 2-byte UTF-8 sequence (11xxxxxx) is a boundary.
//    'é' U+00E9 encodes as 0xC3 0xA9.
#[test]
fn utf8_2byte_lead_is_boundary() {
    let chars = chars_with_encoding(b"\xC3\xA9".to_vec(), UTF_8); // é
    assert!(chars.is_boundary(0).unwrap(), "lead byte is a boundary");
}

// 3. The continuation byte of a 2-byte sequence (10xxxxxx) is NOT a boundary.
#[test]
fn utf8_2byte_continuation_not_boundary() {
    let chars = chars_with_encoding(b"\xC3\xA9".to_vec(), UTF_8); // é
    assert!(
        !chars.is_boundary(1).unwrap(),
        "continuation byte is not a boundary"
    );
}

// 4. The lead byte of a 3-byte UTF-8 sequence is a boundary.
//    '世' U+4E16 encodes as 0xE4 0xB8 0x96.
#[test]
fn utf8_3byte_lead_is_boundary() {
    let chars = chars_with_encoding(b"\xE4\xB8\x96".to_vec(), UTF_8); // 世
    assert!(chars.is_boundary(0).unwrap());
}

// 5. Both continuation bytes of a 3-byte sequence are NOT boundaries.
#[test]
fn utf8_3byte_continuation_not_boundary() {
    let chars = chars_with_encoding(b"\xE4\xB8\x96".to_vec(), UTF_8); // 世
    assert!(
        !chars.is_boundary(1).unwrap(),
        "first continuation not a boundary"
    );
    assert!(
        !chars.is_boundary(2).unwrap(),
        "second continuation not a boundary"
    );
}

// 6. The lead byte of a 4-byte UTF-8 sequence is a boundary.
//    '😀' U+1F600 encodes as 0xF0 0x9F 0x98 0x80.
#[test]
fn utf8_4byte_lead_is_boundary() {
    let chars = chars_with_encoding(b"\xF0\x9F\x98\x80".to_vec(), UTF_8); // 😀
    assert!(chars.is_boundary(0).unwrap());
}

// 7. All three continuation bytes of a 4-byte sequence are NOT boundaries.
#[test]
fn utf8_4byte_continuation_not_boundary() {
    let chars = chars_with_encoding(b"\xF0\x9F\x98\x80".to_vec(), UTF_8); // 😀
    assert!(!chars.is_boundary(1).unwrap());
    assert!(!chars.is_boundary(2).unwrap());
    assert!(!chars.is_boundary(3).unwrap());
}

// 8. Mixed sequence: ASCII 'A', then 2-byte 'é', then 1-byte '\n'.
//    Boundary offsets: 0, 1, 3.  Non-boundaries: 2.
#[test]
fn utf8_mixed_sequence_boundaries() {
    // A=0x41 (1 byte), é=0xC3 0xA9 (2 bytes), \n=0x0A (1 byte)
    let content = b"A\xC3\xA9\n".to_vec();
    let chars = chars_with_encoding(content, UTF_8);
    assert!(chars.is_boundary(0).unwrap(), "A at 0 is boundary");
    assert!(chars.is_boundary(1).unwrap(), "é lead at 1 is boundary");
    assert!(
        !chars.is_boundary(2).unwrap(),
        "é cont at 2 is not boundary"
    );
    assert!(chars.is_boundary(3).unwrap(), "\\n at 3 is boundary");
}

// ── UTF-16LE is_boundary tests ────────────────────────────────────────────────

// 9. A BMP character in UTF-16LE at an even offset is a boundary.
//    'A' U+0041 in UTF-16LE = [0x41, 0x00].  High byte = 0x00, not a low surrogate.
#[test]
fn utf16le_bmp_char_is_boundary() {
    let chars = chars_with_encoding(vec![0x41, 0x00], UTF_16LE);
    assert!(chars.is_boundary(0).unwrap());
}

// 10. Any odd byte offset in a UTF-16LE stream is NOT a character boundary.
#[test]
fn utf16le_odd_offset_not_boundary() {
    let chars = chars_with_encoding(vec![0x41, 0x00, 0x42, 0x00], UTF_16LE);
    assert!(!chars.is_boundary(1).unwrap());
    assert!(!chars.is_boundary(3).unwrap());
}

// 11. A high surrogate code unit in UTF-16LE IS a character boundary (it is the
//     start of a surrogate pair for a supplementary code point).
//     '😀' U+1F600 in UTF-16LE:
//       High surrogate U+D83D → bytes [0x3D, 0xD8]  (high byte = 0xD8, not in DC..DF)
//       Low  surrogate U+DE00 → bytes [0x00, 0xDE]  (high byte = 0xDE, in DC..DF)
#[test]
fn utf16le_surrogate_pair_high_is_boundary() {
    let content = vec![0x3D, 0xD8, 0x00, 0xDE]; // 😀 in UTF-16LE
    let chars = chars_with_encoding(content, UTF_16LE);
    assert!(
        chars.is_boundary(0).unwrap(),
        "high surrogate at 0 is a boundary"
    );
}

// 12. The low surrogate code unit (the second word of a surrogate pair) is NOT
//     a character boundary — the character started at the high surrogate.
#[test]
fn utf16le_surrogate_pair_low_not_boundary() {
    let content = vec![0x3D, 0xD8, 0x00, 0xDE]; // 😀 in UTF-16LE
    let chars = chars_with_encoding(content, UTF_16LE);
    assert!(
        !chars.is_boundary(2).unwrap(),
        "low surrogate at 2 is not a boundary"
    );
}

// 13. A high surrogate in UTF-16BE IS a character boundary.
//     '😀' U+1F600 in UTF-16BE:
//       High surrogate U+D83D → bytes [0xD8, 0x3D]  (first byte = 0xD8, NOT in DC..DF)
//       Low  surrogate U+DE00 → bytes [0xDE, 0x00]  (first byte = 0xDE, in DC..DF)
#[test]
fn utf16be_surrogate_pair_high_is_boundary() {
    let content = vec![0xD8, 0x3D, 0xDE, 0x00]; // 😀 in UTF-16BE
    let chars = chars_with_encoding(content, UTF_16BE);
    assert!(
        chars.is_boundary(0).unwrap(),
        "high surrogate at 0 is a boundary"
    );
}

// 14. Low surrogate in UTF-16BE is NOT a boundary.
#[test]
fn utf16be_surrogate_pair_low_not_boundary() {
    let content = vec![0xD8, 0x3D, 0xDE, 0x00]; // 😀 in UTF-16BE
    let chars = chars_with_encoding(content, UTF_16BE);
    assert!(
        !chars.is_boundary(2).unwrap(),
        "low surrogate at 2 is not a boundary"
    );
}

// ── DBCS (Shift_JIS) tests ────────────────────────────────────────────────────

// Helper: build content = [DBCS 2-byte char 'あ' in Shift_JIS] + suffix.
// 'あ' U+3042 in Shift_JIS = 0x82 0xA0.
fn shift_jis_content() -> Vec<u8> {
    // [0x82, 0xA0] = 'あ' in Shift_JIS, then LF, then CR, then ASCII 'X'
    vec![0x82, 0xA0, 0x0A, 0x0D, 0x58]
}

fn shift_jis_chars() -> Chars {
    chars_with_encoding(shift_jis_content(), encoding_rs::SHIFT_JIS)
}

// 15. A LF byte in a DBCS stream is always a character boundary.
#[test]
fn dbcs_lf_is_boundary() {
    let chars = shift_jis_chars();
    // LF is at offset 2.
    assert!(
        chars.is_boundary(2).unwrap(),
        "LF at offset 2 is a boundary"
    );
}

// 16. A CR byte in a DBCS stream is always a character boundary.
#[test]
fn dbcs_cr_is_boundary() {
    let chars = shift_jis_chars();
    // CR is at offset 3.
    assert!(
        chars.is_boundary(3).unwrap(),
        "CR at offset 3 is a boundary"
    );
}

// 17. A trail byte of a DBCS 2-byte sequence is NOT a boundary.
//     Offset 1 is the trail byte of 'あ' (0x82 [0xA0]).  nearest_sync_point(1)
//     returns 0 (the seeded starting boundary), which is ≠ 1.
#[test]
fn dbcs_trail_byte_not_boundary() {
    let chars = shift_jis_chars();
    // Trail byte of 0x82A0 is at offset 1.
    assert!(
        !chars.is_boundary(1).unwrap(),
        "trail byte at offset 1 is not a boundary"
    );
}

// 18. nearest_sync_point from a trail byte walks back to the nearest anchor.
//     Content: [0x82, 0xA0, 0x0A, ...]
//     Seed: [0].  Scanning to offset 1 finds no anchor ⇒ index stays [0].
//     nearest_sync_point(1) = 0.
#[test]
fn dbcs_nearest_sync_point_from_trail_returns_prior_anchor() {
    let chars = shift_jis_chars();
    assert_eq!(chars.nearest_sync_point(1).unwrap(), 0);
}

// 19. nearest_sync_point from offset 0 returns 0 (the seed itself).
#[test]
fn dbcs_nearest_sync_point_at_start() {
    let chars = shift_jis_chars();
    assert_eq!(chars.nearest_sync_point(0).unwrap(), 0);
}

// 20. nearest_sync_point from after a LF returns the LF offset.
#[test]
fn dbcs_nearest_sync_point_after_lf_returns_lf() {
    let chars = shift_jis_chars();
    // 'X' at offset 4 is past the LF at 2 and CR at 3.
    // LF at 2 and CR at 3 are both in the index.  nearest_sync_point(4) = 3 (CR).
    let sp = chars.nearest_sync_point(4).unwrap();
    assert_eq!(sp, 3, "nearest sync point for offset 4 is CR at offset 3");
}

// ── chars_from tests ──────────────────────────────────────────────────────────

// 21. chars_from at offset 0 in a UTF-8 stream yields the correct characters.
#[test]
fn chars_from_utf8_ascii_yields_correct_chars() {
    let chars = chars_with_encoding(b"hello".to_vec(), UTF_8);
    let result: String = chars.chars_from(0).collect();
    assert_eq!(result, "hello");
}

// 22. chars_from in a UTF-8 stream with multi-byte characters.
#[test]
fn chars_from_utf8_multibyte_chars() {
    // 'A' + é (2-byte) + 世 (3-byte) + 😀 (4-byte)
    let input = "Aé世😀";
    let chars = chars_with_encoding(input.as_bytes().to_vec(), UTF_8);
    let result: String = chars.chars_from(0).collect();
    assert_eq!(result, input);
}

// 23. chars_from starting at a non-zero sync point yields chars from that point.
#[test]
fn chars_from_utf8_non_zero_start() {
    // "AB" — start at byte offset 1 (the 'B').
    let chars = chars_with_encoding(b"AB".to_vec(), UTF_8);
    let result: String = chars.chars_from(1).collect();
    assert_eq!(result, "B");
}

// 24. chars_from past end of branch yields empty iterator.
#[test]
fn chars_from_past_end_is_empty() {
    let chars = chars_with_encoding(b"X".to_vec(), UTF_8);
    let result: Vec<char> = chars.chars_from(999).collect();
    assert!(result.is_empty());
}

// 25. chars_from in UTF-16LE (no BOM, hint supplied) yields correct chars.
#[test]
fn chars_from_utf16le_ascii_yields_correct_chars() {
    // "hi" as UTF-16LE: 'h'=[0x68,0x00], 'i'=[0x69,0x00]
    let content = vec![0x68, 0x00, 0x69, 0x00];
    let chars = chars_with_encoding(content, UTF_16LE);
    let result: String = chars.chars_from(0).collect();
    assert_eq!(result, "hi");
}

// 26. chars_from in Shift_JIS starting at a known anchor (LF) yields correct
//     chars from that point onward.
#[test]
fn chars_from_dbcs_from_anchor_yields_correct_chars() {
    // [0x82, 0xA0, LF, 'X'] — start from LF at offset 2.
    let content = vec![0x82, 0xA0, 0x0A, 0x58]; // 'あ', LF, 'X'
    let chars = chars_with_encoding(content, encoding_rs::SHIFT_JIS);
    let result: String = chars.chars_from(2).collect();
    assert_eq!(result, "\nX");
}

// ── encode round-trip tests ───────────────────────────────────────────────────

// 27. encode round-trips ASCII via UTF-8.
#[test]
fn encode_round_trips_ascii_utf8() {
    let chars = chars_with_encoding(b"hello".to_vec(), UTF_8);
    let bytes = chars.encode("hello").expect("encode");
    assert_eq!(bytes, b"hello");
}

// 28. encode round-trips non-ASCII: 'é' in WINDOWS_1252.
//     'é' (U+00E9) → 0xE9 in windows-1252; decoding 0xE9 in windows-1252 gives 'é'.
#[test]
fn encode_round_trips_non_ascii_latin1() {
    let chars = chars_with_encoding(b"caf\xE9".to_vec(), WINDOWS_1252);
    let bytes = chars.encode("café").expect("encode");
    assert_eq!(bytes, b"caf\xE9");
    // Decode back via encoding_rs to verify the round-trip.
    let (decoded, _enc, had_errors) = WINDOWS_1252.decode(&bytes);
    assert!(!had_errors, "decode should not report errors");
    assert_eq!(decoded.as_ref(), "café");
}

// 29. encode returns Err for a character that has no encoding in the target.
//     WINDOWS_1252 cannot encode CJK characters.
#[test]
fn encode_returns_err_for_unmappable_char() {
    let chars = chars_with_encoding(b"x".to_vec(), WINDOWS_1252);
    let result = chars.encode("世界");
    assert!(result.is_err(), "CJK in latin-1 must fail");
}

// 30. encode round-trips ASCII in Shift_JIS (ASCII is a proper subset).
#[test]
fn encode_round_trips_ascii_shift_jis() {
    let chars = chars_with_encoding(b"hello\n".to_vec(), encoding_rs::SHIFT_JIS);
    let bytes = chars.encode("hello\n").expect("encode");
    assert_eq!(bytes, b"hello\n");
}
