// Copyright (c) 2026, Michael Grier

//! Unit tests for `line_edit`.
//!
//! Three groups:
//! - **Mechanism** — `line_span` (content-vs-terminator boundary in *source*
//!   bytes), `apply` (batched verbatim source splices), `encode_line`, and
//!   BOM handling.
//! - **UTF-16** — the payoff of building on the encoding-aware `Lines`
//!   scanner: spans, terminators, deletes, and content splices are correct on
//!   2-byte code units where a naive `0x0A` scan would mis-slice.
//! - **Policy composition** — a small caller-side layer (the kind that lives
//!   in a consumer such as `tpu`) built purely on the mechanism, proving it is
//!   sufficient to implement a strict, weld-free line editor.

use std::sync::Arc;

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, WINDOWS_1252};
use redwing::{Branch, make_thicket_from_bytes, materialize};

use crate::{
    encoded::EncodeError,
    encoding::{LineEnding, SourceConfig},
    line_edit::{LineEditError, LineEditor, LineSpan, Splice},
    source::Source,
};

// ── helpers ──────────────────────────────────────────────────────────────────

fn editor(bytes: &[u8]) -> LineEditor {
    let branch: Arc<dyn Branch> = make_thicket_from_bytes(bytes.to_vec()).main();
    let source = Source::new(branch, SourceConfig::default()).unwrap();
    source.as_line_editor().unwrap()
}

fn editor_enc(bytes: &[u8], enc: &'static Encoding) -> LineEditor {
    let branch: Arc<dyn Branch> = make_thicket_from_bytes(bytes.to_vec()).main();
    let config = SourceConfig {
        encoding_hint: Some(enc),
        ..SourceConfig::default()
    };
    let source = Source::new(branch, config).unwrap();
    source.as_line_editor().unwrap()
}

fn mat(branch: &Arc<dyn Branch>) -> Vec<u8> {
    materialize(branch.as_ref()).unwrap()
}

/// Encode `s` as UTF-16LE code-unit bytes.
fn u16le(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

// ══ Mechanism: line_count / trailing terminator ══════════════════════════════

#[test]
fn line_count_ignores_trailing_newline() {
    assert_eq!(editor(b"").line_count(), 0);
    assert_eq!(editor(b"a\nb\n").line_count(), 2);
    assert_eq!(editor(b"a\nb").line_count(), 2);
    assert_eq!(editor(b"aaa\nbbb\nccc").line_count(), 3);
}

#[test]
fn trailing_terminated_reflects_final_newline() {
    assert!(editor(b"a\nb\n").is_trailing_terminated());
    assert!(!editor(b"a\nb").is_trailing_terminated());
    assert!(!editor(b"").is_trailing_terminated());
}

#[test]
fn byte_len_reflects_source_length() {
    assert_eq!(editor(b"").byte_len(), 0);
    assert_eq!(editor(b"a\nb\n").byte_len(), 4);
    assert_eq!(editor(b"a\r\nb\r\n").byte_len(), 6);
}

// ══ Mechanism: line_span (source coordinates) ════════════════════════════════

#[test]
fn line_span_interior_line_splits_content_from_terminator() {
    // "A\nB\nC\n": line 1 is "B" at [2,3); its LF terminator at [3,4).
    let ed = editor(b"A\nB\nC\n");
    assert_eq!(
        ed.line_span(1..2).unwrap(),
        LineSpan {
            content: 2..3,
            full: 2..4,
            terminated: true,
        }
    );
}

#[test]
fn line_span_crlf_terminator_is_two_source_bytes() {
    // "A\r\nB\r\n": the CRLF occupies two *source* bytes, so `full` extends
    // past `content` by two.
    let ed = editor(b"A\r\nB\r\n");
    assert_eq!(
        ed.line_span(0..1).unwrap(),
        LineSpan {
            content: 0..1,
            full: 0..3,
            terminated: true,
        }
    );
    assert_eq!(
        ed.line_span(1..2).unwrap(),
        LineSpan {
            content: 3..4,
            full: 3..6,
            terminated: true,
        }
    );
}

#[test]
fn line_span_unterminated_final_line() {
    // "a\nb": final line "b" at [2,3) with no terminator.
    let ed = editor(b"a\nb");
    assert_eq!(
        ed.line_span(1..2).unwrap(),
        LineSpan {
            content: 2..3,
            full: 2..3,
            terminated: false,
        }
    );
}

#[test]
fn line_span_multi_line_range_spans_to_last_terminator() {
    // "A\nB\nC\n": lines 0..2 cover "A\nB" plus the second LF.
    let ed = editor(b"A\nB\nC\n");
    assert_eq!(
        ed.line_span(0..2).unwrap(),
        LineSpan {
            content: 0..3,
            full: 0..4,
            terminated: true,
        }
    );
}

#[test]
fn line_span_empty_range_is_insertion_point() {
    let ed = editor(b"a\nb\n");
    // Before line 1: zero-width point at the start of line 1.
    assert_eq!(
        ed.line_span(1..1).unwrap(),
        LineSpan {
            content: 2..2,
            full: 2..2,
            terminated: false,
        }
    );
    // At end of document (index == line_count): point at total length.
    assert_eq!(
        ed.line_span(2..2).unwrap(),
        LineSpan {
            content: 4..4,
            full: 4..4,
            terminated: false,
        }
    );
}

#[test]
fn line_span_out_of_range_errors() {
    let ed = editor(b"a\nb\n");
    assert_eq!(
        ed.line_span(0..3),
        Err(LineEditError::LineOutOfRange {
            index: 3,
            line_count: 2,
        })
    );
}

#[test]
fn line_span_inverted_range_errors() {
    let ed = editor(b"a\nb\n");
    // Build the inverted range via the struct literal to avoid the
    // `reversed_empty_ranges` lint on a `2..1` literal.
    let inverted = std::ops::Range { start: 2, end: 1 };
    assert_eq!(
        ed.line_span(inverted),
        Err(LineEditError::LineRangeInverted { start: 2, end: 1 })
    );
}

// ══ Mechanism: apply ═════════════════════════════════════════════════════════

#[test]
fn apply_content_splice_preserves_terminator() {
    let ed = editor(b"A\nB\nC\n");
    let span = ed.line_span(1..2).unwrap();
    let out = ed
        .apply(&[Splice {
            range: span.content,
            replacement: b"BB".to_vec(),
        }])
        .unwrap();
    assert_eq!(mat(&out), b"A\nBB\nC\n");
}

#[test]
fn apply_full_splice_deletes_line_and_terminator() {
    let ed = editor(b"A\nB\nC\n");
    let span = ed.line_span(1..2).unwrap();
    let out = ed
        .apply(&[Splice {
            range: span.full,
            replacement: Vec::new(),
        }])
        .unwrap();
    assert_eq!(mat(&out), b"A\nC\n");
}

#[test]
fn apply_multiple_splices_any_order() {
    let ed = editor(b"A\nB\nC\n");
    let s0 = ed.line_span(0..1).unwrap();
    let s2 = ed.line_span(2..3).unwrap();
    // Supplied out of order; `apply` sorts internally.
    let out = ed
        .apply(&[
            Splice {
                range: s2.content,
                replacement: b"CC".to_vec(),
            },
            Splice {
                range: s0.content,
                replacement: b"AA".to_vec(),
            },
        ])
        .unwrap();
    assert_eq!(mat(&out), b"AA\nB\nCC\n");
}

#[test]
fn from_source_is_isolated_from_later_mutation_of_shared_branch() {
    // `from_source` forks an immutable snapshot rather than retaining the
    // caller's `Arc<dyn Branch>` directly, so a later mutation through
    // another handle to the same branch must not corrupt the editor's
    // already-cached line offsets or `apply`'s output.
    let branch: Arc<dyn Branch> = make_thicket_from_bytes(b"A\nB\nC\n".to_vec()).main();
    let source = Source::new(Arc::clone(&branch), SourceConfig::default()).unwrap();
    let ed = source.as_line_editor().unwrap();

    // Mutate the document through the caller's retained handle.
    branch.insert_before(0, b"Z").unwrap();

    // The cached line map still describes the pre-mutation content, so a
    // no-op apply must reproduce it exactly rather than reading through
    // stale offsets into the now-mutated branch.
    let out = ed.apply(&[]).unwrap();
    assert_eq!(mat(&out), b"A\nB\nC\n");
}

#[test]
fn apply_same_start_splices_are_order_independent() {
    // A zero-width insertion and a range replacement anchored at the same
    // offset must produce the same result (and neither be rejected as
    // overlapping) regardless of the order they're supplied in.
    let insert = Splice {
        range: 2..2,
        replacement: b"X".to_vec(),
    };
    let replace = Splice {
        range: 2..4,
        replacement: b"Y".to_vec(),
    };

    let ed = editor(b"A\nB\nC\n");
    let out_a = ed.apply(&[insert.clone(), replace.clone()]).unwrap();
    assert_eq!(mat(&out_a), b"A\nXYC\n");

    let ed = editor(b"A\nB\nC\n");
    let out_b = ed.apply(&[replace, insert]).unwrap();
    assert_eq!(mat(&out_b), b"A\nXYC\n");
}

#[test]
fn apply_two_zero_width_splices_at_same_offset_errors() {
    // Two zero-width insertions anchored at the exact same offset have no
    // well-defined relative order, so `apply` rejects them as overlapping
    // rather than silently resolving them by caller-supplied order.
    let a = Splice {
        range: 2..2,
        replacement: b"X".to_vec(),
    };
    let b = Splice {
        range: 2..2,
        replacement: b"Y".to_vec(),
    };

    let ed = editor(b"A\nB\nC\n");
    let result = ed.apply(&[a.clone(), b.clone()]);
    assert!(matches!(result, Err(LineEditError::SpliceOverlap { .. })));

    // Same result regardless of input order.
    let ed = editor(b"A\nB\nC\n");
    let result = ed.apply(&[b, a]);
    assert!(matches!(result, Err(LineEditError::SpliceOverlap { .. })));
}

#[test]
fn apply_overlap_errors() {
    let ed = editor(b"A\nB\nC\n");
    let result = ed.apply(&[
        Splice {
            range: 0..3,
            replacement: Vec::new(),
        },
        Splice {
            range: 2..4,
            replacement: Vec::new(),
        },
    ]);
    assert!(matches!(result, Err(LineEditError::SpliceOverlap { .. })));
}

#[test]
fn apply_out_of_bounds_errors() {
    let ed = editor(b"A\nB\n");
    let result = ed.apply(&[Splice {
        range: 0..99,
        replacement: Vec::new(),
    }]);
    assert!(matches!(
        result,
        Err(LineEditError::SpliceOutOfBounds { .. })
    ));
}

#[test]
fn apply_splice_ending_exactly_at_document_end_is_valid() {
    // A splice range whose `end` lands exactly on the document's byte
    // length (touching true EOF) must be accepted, not rejected as
    // out-of-bounds: only `end > total_len` is invalid.
    let ed = editor(b"A\nB\n");
    let span = ed.line_span(1..2).unwrap(); // "B\n", full.end == byte_len() == 4
    assert_eq!(span.full.end, ed.byte_len());
    let out = ed
        .apply(&[Splice {
            range: span.full,
            replacement: b"X\n".to_vec(),
        }])
        .unwrap();
    assert_eq!(mat(&out), b"A\nX\n");
}

// ══ Mechanism: LineEditError / LineEditor Display, Debug, source ═════════════

#[test]
fn line_edit_error_display_messages() {
    assert_eq!(
        LineEditError::LineOutOfRange {
            index: 3,
            line_count: 2,
        }
        .to_string(),
        "line index 3 out of range (document has 2 lines)"
    );
    assert_eq!(
        LineEditError::LineRangeInverted { start: 2, end: 1 }.to_string(),
        "inverted line range 2..1"
    );
    assert_eq!(
        LineEditError::SpliceOutOfBounds {
            start: 0,
            end: 99,
            len: 4,
        }
        .to_string(),
        "splice 0..99 out of bounds (document is 4 bytes)"
    );
    assert_eq!(
        LineEditError::SpliceOverlap {
            first_end: 4,
            second_start: 2,
        }
        .to_string(),
        "overlapping splices: 4 > 2"
    );
    // Same-point zero-width tie: `first_end == second_start`, so there's no
    // genuine `>` relation to report — Display must not claim one.
    assert_eq!(
        LineEditError::SpliceOverlap {
            first_end: 2,
            second_start: 2,
        }
        .to_string(),
        "overlapping splices: two zero-width splices anchored at the same offset (2) \
         have no defined relative order"
    );
    assert_eq!(
        LineEditError::EncodeUnavailable {
            encoding_name: "utf-16le",
        }
        .to_string(),
        "encoding 'utf-16le' has no encoder; supply pre-encoded bytes"
    );
    assert_eq!(
        LineEditError::Io {
            kind: std::io::ErrorKind::Other,
        }
        .to_string(),
        "branch splice failed: other error"
    );
    assert_eq!(
        LineEditError::TruncatedScan {
            scanned: 4,
            expected: 6,
        }
        .to_string(),
        "line scan covered 4 of 6 source bytes (stopped early, likely an I/O error)"
    );
}

#[test]
fn line_edit_error_source_chains_encode_only() {
    use std::error::Error;

    let encode_err = LineEditError::Encode(EncodeError::Unmappable {
        encoding_name: "windows-1252",
    });
    assert!(encode_err.source().is_some());

    assert!(
        LineEditError::LineOutOfRange {
            index: 0,
            line_count: 0,
        }
        .source()
        .is_none()
    );
}

#[test]
fn line_editor_debug_includes_key_fields() {
    let ed = editor(b"A\nB\nC\n");
    let debug = format!("{ed:?}");
    assert!(debug.contains("LineEditor"));
    assert!(debug.contains("line_count"));
    assert!(debug.contains('3')); // line_count == 3
}

#[test]
fn encode_line_utf8_normalises_embedded_endings() {
    let ed = editor(b"a\n");
    assert_eq!(ed.encode_line("x\r\ny").unwrap(), b"x\ny");
}

#[test]
fn encode_line_respects_crlf_document_for_embedded_newlines() {
    // A CRLF document must not get mixed terminators: an embedded `\n` in
    // multi-line replacement text is normalised, then re-terminated with the
    // document's own convention rather than a hard-coded LF.
    let ed = editor(b"A\r\nB\r\n");
    assert_eq!(ed.encode_line("x\ny").unwrap(), b"x\r\ny");
    // Already-CRLF input round-trips rather than doubling the `\r`.
    assert_eq!(ed.encode_line("x\r\ny").unwrap(), b"x\r\ny");
}

#[test]
fn encode_line_windows1252_unmappable_errors() {
    // An emoji has no windows-1252 mapping.
    let ed = editor_enc(b"a\n", WINDOWS_1252);
    assert!(matches!(
        ed.encode_line("\u{1F600}"),
        Err(LineEditError::Encode(_))
    ));
}

// ══ UTF-16: the payoff ═══════════════════════════════════════════════════════

#[test]
fn utf16le_spans_count_two_byte_code_units() {
    // BOM + "a\nb\n" in UTF-16LE.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(u16le("a\nb\n"));
    let ed = editor_enc(&bytes, UTF_16LE);

    assert_eq!(ed.line_count(), 2);
    assert!(ed.is_trailing_terminated());
    // Line 0: payload "a" [2,4), LF unit [4,6).
    assert_eq!(
        ed.line_span(0..1).unwrap(),
        LineSpan {
            content: 2..4,
            full: 2..6,
            terminated: true,
        }
    );
    // Line 1: payload "b" [6,8), LF unit [8,10).
    assert_eq!(
        ed.line_span(1..2).unwrap(),
        LineSpan {
            content: 6..8,
            full: 6..10,
            terminated: true,
        }
    );
}

#[test]
fn utf16le_crlf_terminator_is_four_source_bytes() {
    // BOM + "a\r\nb\r\n" in UTF-16LE: each CRLF terminator is two 2-byte code
    // units (4 source bytes), not two 1-byte units — a naive `unit_size *
    // code_unit_count` computed as division instead of multiplication would
    // still happen to give the right answer for a bare LF (`1 * unit_size ==
    // unit_size` either way when unit_size == 1) but not here.
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(u16le("a\r\nb\r\n"));
    let ed = editor_enc(&bytes, UTF_16LE);

    assert_eq!(ed.line_count(), 2);
    // Line 0: payload "a" [2,4), CRLF unit [4,8) (4 bytes: CR unit + LF unit).
    assert_eq!(
        ed.line_span(0..1).unwrap(),
        LineSpan {
            content: 2..4,
            full: 2..8,
            terminated: true,
        }
    );
    assert_eq!(ed.byte_len(), 14); // BOM(2) + ("a"=2 + CRLF=4) + ("b"=2 + CRLF=4)
}


#[test]
fn utf16le_terminator_is_a_code_unit() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(u16le("a\nb\n"));
    let ed = editor_enc(&bytes, UTF_16LE);
    assert_eq!(ed.terminator(LineEnding::Lf), vec![0x0A, 0x00]);
    assert_eq!(ed.terminator(LineEnding::Cr), vec![0x0D, 0x00]);
    assert_eq!(
        ed.terminator(LineEnding::CrLf),
        vec![0x0D, 0x00, 0x0A, 0x00]
    );
}

#[test]
fn utf16_encode_line_is_unavailable() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(u16le("a\n"));
    let ed = editor_enc(&bytes, UTF_16LE);
    assert!(matches!(
        ed.encode_line("x"),
        Err(LineEditError::EncodeUnavailable { .. })
    ));
}

#[test]
fn utf16le_delete_line_via_full_span() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(u16le("a\nb\n"));
    let ed = editor_enc(&bytes, UTF_16LE);
    let span = ed.line_span(0..1).unwrap();
    let out = ed
        .apply(&[Splice {
            range: span.full,
            replacement: Vec::new(),
        }])
        .unwrap();
    let mut expected = vec![0xFF, 0xFE];
    expected.extend(u16le("b\n"));
    assert_eq!(mat(&out), expected);
}

#[test]
fn utf16le_content_splice_keeps_terminator() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(u16le("a\nb\n"));
    let ed = editor_enc(&bytes, UTF_16LE);
    let span = ed.line_span(0..1).unwrap();
    // Caller supplies pre-encoded UTF-16LE bytes for "X".
    let out = ed
        .apply(&[Splice {
            range: span.content,
            replacement: u16le("X"),
        }])
        .unwrap();
    let mut expected = vec![0xFF, 0xFE];
    expected.extend(u16le("X\nb\n"));
    assert_eq!(mat(&out), expected);
}

#[test]
fn utf16be_spans_count_two_byte_code_units() {
    // BOM (FE FF) + "a\n" in UTF-16BE.
    let bytes = vec![0xFE, 0xFF, 0x00, 0x61, 0x00, 0x0A];
    let ed = editor_enc(&bytes, UTF_16BE);
    assert_eq!(ed.line_count(), 1);
    assert_eq!(
        ed.line_span(0..1).unwrap(),
        LineSpan {
            content: 2..4,
            full: 2..6,
            terminated: true,
        }
    );
    assert_eq!(ed.terminator(LineEnding::Lf), vec![0x00, 0x0A]);
}

// ══ Policy composition ═══════════════════════════════════════════════════════
//
// A minimal weld-free line-editing policy built purely on the mechanism, of
// the kind a consumer (e.g. `tpu`) would supply.

/// Replace the content of a line range, preserving the range's terminator.
fn policy_replace(
    ed: &LineEditor,
    lines: std::ops::Range<usize>,
    text: &str,
) -> Result<Splice, LineEditError> {
    let span = ed.line_span(lines)?;
    Ok(Splice {
        range: span.content,
        replacement: ed.encode_line(text)?,
    })
}

/// Insert a new, terminated line before `line`.
fn policy_insert_before(ed: &LineEditor, line: usize, text: &str) -> Result<Splice, LineEditError> {
    let span = ed.line_span(line..line)?;
    let mut replacement = ed.encode_line(text)?;
    replacement.extend(ed.terminator(ed.line_ending()));
    Ok(Splice {
        range: span.content,
        replacement,
    })
}

/// Delete a line range including its terminator.
fn policy_delete(ed: &LineEditor, lines: std::ops::Range<usize>) -> Result<Splice, LineEditError> {
    let span = ed.line_span(lines)?;
    Ok(Splice {
        range: span.full,
        replacement: Vec::new(),
    })
}

#[test]
fn policy_replace_does_not_weld() {
    let ed = editor(b"A\nB\nC\n");
    let splice = policy_replace(&ed, 1..2, "BB").unwrap();
    let out = ed.apply(&[splice]).unwrap();
    assert_eq!(mat(&out), b"A\nBB\nC\n");
}

#[test]
fn policy_replace_multiline_text_stays_terminated() {
    // Replacing "B" with "X\nY" keeps line 1's original terminator, so the
    // result is well-formed rather than welded.
    let ed = editor(b"A\nB\nC\n");
    let splice = policy_replace(&ed, 1..2, "X\nY").unwrap();
    let out = ed.apply(&[splice]).unwrap();
    assert_eq!(mat(&out), b"A\nX\nY\nC\n");
}

#[test]
fn policy_replace_multiline_text_uses_document_terminator() {
    // Same scenario as `policy_replace_multiline_text_stays_terminated`, but
    // in a CRLF document: the embedded newline in the replacement must also
    // become CRLF, not a bare LF, so the whole file stays consistently
    // terminated.
    let ed = editor(b"A\r\nB\r\nC\r\n");
    let splice = policy_replace(&ed, 1..2, "X\nY").unwrap();
    let out = ed.apply(&[splice]).unwrap();
    assert_eq!(mat(&out), b"A\r\nX\r\nY\r\nC\r\n");
}

#[test]
fn policy_insert_before_line() {
    let ed = editor(b"A\nB\n");
    let splice = policy_insert_before(&ed, 1, "NEW").unwrap();
    let out = ed.apply(&[splice]).unwrap();
    assert_eq!(mat(&out), b"A\nNEW\nB\n");
}

#[test]
fn policy_delete_range() {
    let ed = editor(b"A\nB\nC\nD\n");
    let splice = policy_delete(&ed, 1..3).unwrap();
    let out = ed.apply(&[splice]).unwrap();
    assert_eq!(mat(&out), b"A\nD\n");
}

#[test]
fn policy_replace_respects_crlf_terminator() {
    // The preserved terminator is the line's own CRLF, taken from the `full`
    // span boundary — content replacement never touches it.
    let ed = editor(b"A\r\nB\r\nC\r\n");
    let splice = policy_replace(&ed, 1..2, "BB").unwrap();
    let out = ed.apply(&[splice]).unwrap();
    assert_eq!(mat(&out), b"A\r\nBB\r\nC\r\n");
}
