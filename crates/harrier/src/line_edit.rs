// Copyright (c) 2026, Michael Grier

//! Line-addressed editing built on the encoding-aware [`Lines`] scanner.
//!
//! # Why this lives in `harrier`
//!
//! Splitting a file into lines, classifying each line's terminator, and
//! re-terminating replacement text are all encoding-sensitive operations.
//! [`Lines`] already does the hard part: it detects line terminators in the
//! source *code units* (single-byte for UTF-8 / legacy encodings, 2-byte for
//! UTF-16LE / UTF-16BE) and reports, per line, the exact [`LineTerminator`]
//! that ended it. `LineEditor` reuses that detection wholesale rather than
//! re-implementing a byte-level `\n` scan, so it is correct for every encoding
//! `harrier` supports — including UTF-16, where a naive "split on `0x0A`" would
//! mis-slice every other code unit.
//!
//! # Mechanism, not policy
//!
//! `LineEditor` is deliberately low-level. It answers two questions and
//! performs one action:
//!
//! - [`line_span`](LineEditor::line_span) — where, in **source bytes**, does a
//!   half-open range of lines live? It reports both the *content* span (payload
//!   only, terminator excluded) and the *full* span (payload plus the line's
//!   own source terminator).
//! - [`terminator`](LineEditor::terminator) / [`encode_line`](LineEditor::encode_line)
//!   — the building blocks a caller needs to synthesise replacement bytes in
//!   the file's own encoding and terminator convention.
//! - [`apply`](LineEditor::apply) — splice a set of caller-provided,
//!   already-source-encoded byte ranges onto a fork of the branch.
//!
//! Higher-level *policy* (what does "replace line N", "insert after line N", or
//! "delete lines A..B" mean; how should a multi-line replacement be
//! re-terminated) lives above this type. Keeping the split here means the
//! welding hazard — accidentally joining two logical lines because the
//! replacement dropped a terminator — is a property of the policy layer's span
//! choice (`content` vs `full`), not a bug baked into the mechanism.
//!
//! # Coordinate space
//!
//! Every offset exposed by this module is a **source byte offset** into the
//! branch (after any byte-order mark). Payload bytes are addressed verbatim; a
//! line's terminator occupies its real source width (2 bytes for `CrLf`, and
//! double that again under UTF-16). This is the coordinate space
//! [`redwing::Branch::splice`] operates in, so [`apply`](LineEditor::apply)
//! needs no translation step.

use std::{ops::Range, sync::Arc};

use encoding_rs::Encoding;
use redwing::Branch;

use crate::{
    denormalise::line_ending_bytes,
    encoded::{EncodeError, encode_with},
    encoding::LineEnding,
    lines::{LineTerminator, LinesError},
    source::Source,
};

/// Per-line source-offset bookkeeping.
///
/// All fields are source byte offsets into the branch. `content_end` excludes
/// the line's terminator; `full_end` includes it. For an unterminated final
/// line `content_end == full_end` and `terminated` is `false`.
#[derive(Debug, Clone, Copy)]
struct LineInfo {
    start: u64,
    content_end: u64,
    full_end: u64,
    terminated: bool,
}

/// The location of a half-open range of lines, in source bytes.
///
/// `content` addresses the payload only (terminator excluded); `full` extends
/// to include the source terminator of the last line in the range. For an
/// empty range both spans are the same zero-width point and `terminated` is
/// `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSpan {
    /// Payload bytes, terminator excluded.
    pub content: Range<u64>,
    /// Payload plus the last line's source terminator.
    pub full: Range<u64>,
    /// Whether the last line in the range carried a source terminator.
    pub terminated: bool,
}

/// A single verbatim byte substitution in source-offset space.
///
/// `replacement` must already be encoded in the file's encoding and carry
/// whatever source terminators the caller intends — [`apply`](LineEditor::apply)
/// writes it byte-for-byte. Build it from [`encode_line`](LineEditor::encode_line)
/// and [`terminator`](LineEditor::terminator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    /// Half-open source byte range to replace.
    pub range: Range<u64>,
    /// Verbatim source-encoded replacement bytes.
    pub replacement: Vec<u8>,
}

/// Errors surfaced by [`LineEditor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineEditError {
    /// A line index was past the end of the document.
    LineOutOfRange {
        /// The offending index.
        index: usize,
        /// The document's line count.
        line_count: usize,
    },
    /// A line range had `start > end`.
    LineRangeInverted {
        /// The requested start.
        start: usize,
        /// The requested end.
        end: usize,
    },
    /// A splice range fell outside the document, or had `start > end`.
    SpliceOutOfBounds {
        /// The requested start.
        start: u64,
        /// The requested end.
        end: u64,
        /// The document's source byte length.
        len: u64,
    },
    /// Two splices in the same [`apply`](LineEditor::apply) batch overlapped.
    SpliceOverlap {
        /// End of the earlier splice.
        first_end: u64,
        /// Start of the later splice.
        second_start: u64,
    },
    /// The document's encoding has no `encoding_rs` encoder, so text cannot be
    /// safely turned into source bytes here.
    ///
    /// This is the UTF-16 case: `encoding_rs` has no UTF-16 *encoder* and would
    /// silently fall back to UTF-8, corrupting the file. Callers must supply
    /// pre-encoded UTF-16 bytes to [`apply`](LineEditor::apply) directly.
    EncodeUnavailable {
        /// The WHATWG name of the encoding.
        encoding_name: &'static str,
    },
    /// A character in the replacement text could not be represented in the
    /// document's encoding.
    Encode(EncodeError),
    /// The underlying branch splice failed.
    Io {
        /// The I/O error kind.
        kind: std::io::ErrorKind,
    },
    /// The line scan stopped before reaching the end of the branch.
    ///
    /// [`crate::lines::Lines`] treats a branch read error as end-of-file
    /// rather than propagating it, so a truncated scan is otherwise
    /// indistinguishable from a genuinely short document. `LineEditor`
    /// detects this by comparing the scanned length against the branch's
    /// actual byte length.
    TruncatedScan {
        /// Source bytes actually covered by the scan.
        scanned: u64,
        /// The branch's actual byte length.
        expected: u64,
    },
}

impl std::fmt::Display for LineEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineEditError::LineOutOfRange { index, line_count } => write!(
                f,
                "line index {index} out of range (document has {line_count} lines)"
            ),
            LineEditError::LineRangeInverted { start, end } => {
                write!(f, "inverted line range {start}..{end}")
            }
            LineEditError::SpliceOutOfBounds { start, end, len } => write!(
                f,
                "splice {start}..{end} out of bounds (document is {len} bytes)"
            ),
            LineEditError::SpliceOverlap {
                first_end,
                second_start,
            } => write!(f, "overlapping splices: {first_end} > {second_start}"),
            LineEditError::EncodeUnavailable { encoding_name } => write!(
                f,
                "encoding '{encoding_name}' has no encoder; supply pre-encoded bytes"
            ),
            LineEditError::Encode(e) => write!(f, "{e}"),
            LineEditError::Io { kind } => write!(f, "branch splice failed: {kind}"),
            LineEditError::TruncatedScan { scanned, expected } => write!(
                f,
                "line scan covered {scanned} of {expected} source bytes (stopped early, likely an I/O error)"
            ),
        }
    }
}

impl std::error::Error for LineEditError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LineEditError::Encode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<EncodeError> for LineEditError {
    fn from(e: EncodeError) -> Self {
        LineEditError::Encode(e)
    }
}

impl From<LinesError> for LineEditError {
    /// `LinesError` can't be embedded directly (it wraps `std::io::Error`,
    /// which isn't `Clone`/`Eq`, unlike `LineEditError`), so it's collapsed
    /// into the I/O variant. `as_lines()` documents itself as currently
    /// infallible, so `RangeExceedsCeiling` is unreachable on this path; it's
    /// still mapped rather than left to panic if that ever changes.
    fn from(e: LinesError) -> Self {
        match e {
            LinesError::Io(io_err) => LineEditError::Io { kind: io_err.kind() },
            LinesError::RangeExceedsCeiling { .. } => LineEditError::Io {
                kind: std::io::ErrorKind::Other,
            },
        }
    }
}

/// Line-addressed editor over a [`Source`]'s branch.
///
/// Construct with [`from_source`](LineEditor::from_source) (or
/// [`Source::as_line_editor`](crate::source::Source::as_line_editor)). The
/// editor is immutable: [`apply`](LineEditor::apply) returns a new branch and
/// leaves `self` untouched, so a single editor can drive many independent
/// edits.
#[derive(Clone)]
pub struct LineEditor {
    branch: Arc<dyn Branch>,
    encoding: &'static Encoding,
    line_ending: LineEnding,
    lines: Vec<LineInfo>,
    /// Source byte length of the branch (BOM included).
    total_len: u64,
}

impl std::fmt::Debug for LineEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineEditor")
            .field("encoding", &self.encoding.name())
            .field("line_ending", &self.line_ending)
            .field("line_count", &self.lines.len())
            .field("total_len", &self.total_len)
            .finish_non_exhaustive()
    }
}

impl LineEditor {
    /// Build an editor by scanning `source` into lines.
    ///
    /// Consumes `source`; the resulting editor borrows nothing from it. The
    /// scan is encoding-aware — line terminators are detected in the source's
    /// code units, so UTF-16 is handled correctly.
    ///
    /// The branch is forked immediately, before scanning, so the cached line
    /// map stays valid even if the caller (or another owner of the same
    /// underlying [`Branch`]) later mutates the branch `from_source` was
    /// built from: [`Branch::fork`](redwing::Branch::fork) takes an immutable
    /// snapshot, and mutations to the original are never visible in a fork.
    pub fn from_source(source: Source) -> Result<Self, LineEditError> {
        let bom_len = source.bom_len() as u64;
        let encoding = source.encoding();
        let line_ending = source.line_ending();
        let unit_size: u64 = if is_utf16(encoding) { 2 } else { 1 };

        let lines_iter = source.as_lines()?;
        let branch = lines_iter.branch().fork();
        let total_len = branch.byte_len();

        let mut lines = Vec::new();
        let mut pos = bom_len;
        for (content, term) in lines_iter {
            let content_len = content.len() as u64;
            let (payload_len, source_term_len, terminated) = match term {
                LineTerminator::Ending(le) => {
                    // `Lines` normalises every terminator to a single LF code
                    // unit that is *included* in `content`; strip it to recover
                    // the payload, then re-add the terminator's true source
                    // width.
                    let payload = content_len.saturating_sub(unit_size);
                    let source_term = match le {
                        LineEnding::Lf | LineEnding::Cr => unit_size,
                        LineEnding::CrLf => 2 * unit_size,
                    };
                    (payload, source_term, true)
                }
                LineTerminator::End => (content_len, 0, false),
            };
            let start = pos;
            let content_end = start + payload_len;
            let full_end = content_end + source_term_len;
            lines.push(LineInfo {
                start,
                content_end,
                full_end,
                terminated,
            });
            pos = full_end;
        }

        // Every post-BOM source byte belongs to exactly one line, so a
        // complete scan always ends with `pos == total_len`. `Lines` treats a
        // branch read error as end-of-file (see `Lines::refill`), so without
        // this check a mid-file I/O error would silently yield a truncated
        // line map instead of surfacing as an error.
        if pos != total_len {
            return Err(LineEditError::TruncatedScan {
                scanned: pos,
                expected: total_len,
            });
        }

        Ok(LineEditor {
            branch,
            encoding,
            line_ending,
            lines,
            total_len,
        })
    }

    /// The number of lines in the document.
    ///
    /// A trailing terminator does not create a phantom empty final line: a file
    /// ending in `"a\n"` has one line.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Whether the final line carries a source terminator.
    ///
    /// `false` for an empty document and for a file whose last line has no
    /// trailing terminator.
    pub fn is_trailing_terminated(&self) -> bool {
        self.lines.last().is_some_and(|l| l.terminated)
    }

    /// The document's dominant line-ending convention, as detected at open.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// The document's source byte length (BOM included).
    pub fn byte_len(&self) -> u64 {
        self.total_len
    }

    /// The document's encoding.
    pub fn encoding(&self) -> &'static Encoding {
        self.encoding
    }

    /// Locate a half-open range of lines in source-byte space.
    ///
    /// `lines.start == lines.end` is an *empty* range and yields a zero-width
    /// point at the start of line `lines.start` (or at end-of-document when
    /// `lines.start == line_count()`) — useful as an insertion anchor.
    pub fn line_span(&self, lines: Range<usize>) -> Result<LineSpan, LineEditError> {
        if lines.start > lines.end {
            return Err(LineEditError::LineRangeInverted {
                start: lines.start,
                end: lines.end,
            });
        }
        let line_count = self.lines.len();
        if lines.end > line_count {
            return Err(LineEditError::LineOutOfRange {
                index: lines.end,
                line_count,
            });
        }

        if lines.start == lines.end {
            let pos = if lines.start < line_count {
                self.lines[lines.start].start
            } else {
                self.total_len
            };
            return Ok(LineSpan {
                content: pos..pos,
                full: pos..pos,
                terminated: false,
            });
        }

        let first = &self.lines[lines.start];
        let last = &self.lines[lines.end - 1];
        Ok(LineSpan {
            content: first.start..last.content_end,
            full: first.start..last.full_end,
            terminated: last.terminated,
        })
    }

    /// Encode `text` into the document's encoding as **content** bytes.
    ///
    /// Any `\r\n` or `\r` in `text` is first normalised to `\n`, then any
    /// embedded newline is rewritten to the document's own
    /// [`line_ending`](LineEditor::line_ending) convention (e.g. `\r\n` for a
    /// CRLF document), so multi-line replacement text can't silently mix
    /// terminators with the rest of the file. To re-terminate a line at the
    /// *end* of the replacement, append [`terminator`](LineEditor::terminator)
    /// separately.
    ///
    /// Returns [`LineEditError::EncodeUnavailable`] for UTF-16 (which has no
    /// `encoding_rs` encoder) and [`LineEditError::Encode`] when a character
    /// has no mapping in the target encoding.
    pub fn encode_line(&self, text: &str) -> Result<Vec<u8>, LineEditError> {
        if is_utf16(self.encoding) {
            return Err(LineEditError::EncodeUnavailable {
                encoding_name: self.encoding.name(),
            });
        }
        let normalised = normalise_lf(text);
        let converted = apply_line_ending(&normalised, self.line_ending);
        Ok(encode_with(self.encoding, &converted)?)
    }

    /// The source bytes of line-ending `le` in the document's encoding.
    ///
    /// Widened to code units for UTF-16 (e.g. `Lf` → `[0x0A, 0x00]` under
    /// UTF-16LE). This is the encoding-correct terminator to append to
    /// [`encode_line`](LineEditor::encode_line) output.
    pub fn terminator(&self, le: LineEnding) -> Vec<u8> {
        line_ending_bytes(le, self.encoding)
    }

    /// Apply a batch of source-space splices to a fork of the branch.
    ///
    /// Splices must not overlap; they may be supplied in any order. Two or
    /// more zero-width splices anchored at the exact same offset are also
    /// rejected as overlapping: nothing in the splice itself orders "insert
    /// X here" relative to "insert Y here" at the same point, so resolving it
    /// silently by caller-supplied order would violate the "any order"
    /// guarantee. Each `replacement` is written verbatim, so the caller is
    /// responsible for encoding and terminating it (see
    /// [`encode_line`](LineEditor::encode_line) and
    /// [`terminator`](LineEditor::terminator)). `self` is untouched; the
    /// returned branch is a new fork.
    pub fn apply(&self, splices: &[Splice]) -> Result<Arc<dyn Branch>, LineEditError> {
        let mut ordered: Vec<&Splice> = splices.iter().collect();
        for s in &ordered {
            if s.range.start > s.range.end || s.range.end > self.total_len {
                return Err(LineEditError::SpliceOutOfBounds {
                    start: s.range.start,
                    end: s.range.end,
                    len: self.total_len,
                });
            }
        }
        // Sort by (start, end) rather than start alone so that splices sharing
        // a start point (e.g. a zero-width insertion and a replacement both
        // anchored at the same offset) are ordered deterministically. Relying
        // on a start-only stable sort would make the overlap check below
        // depend on the caller-supplied order of same-start splices, which
        // contradicts the "may be supplied in any order" contract above.
        ordered.sort_by_key(|s| (s.range.start, s.range.end));
        for pair in ordered.windows(2) {
            let both_zero_width_at_same_point = pair[0].range.start == pair[0].range.end
                && pair[1].range.start == pair[1].range.end
                && pair[0].range.start == pair[1].range.start;
            if both_zero_width_at_same_point || pair[0].range.end > pair[1].range.start {
                return Err(LineEditError::SpliceOverlap {
                    first_end: pair[0].range.end,
                    second_start: pair[1].range.start,
                });
            }
        }

        let fork = self.branch.fork();
        // Apply right-to-left so earlier offsets stay valid as later ranges are
        // rewritten.
        for s in ordered.iter().rev() {
            fork.splice(s.range.start, s.range.end - s.range.start, &s.replacement)
                .map_err(|e| LineEditError::Io { kind: e.kind() })?;
        }
        Ok(fork)
    }
}

/// Whether `encoding` is one of the two UTF-16 variants.
fn is_utf16(encoding: &'static Encoding) -> bool {
    encoding == encoding_rs::UTF_16LE || encoding == encoding_rs::UTF_16BE
}

/// Rewrite every `\n` in an already-LF-normalised `text` to the source-byte
/// representation of `le` (`\n` for `Lf`, `\r` for `Cr`, `\r\n` for `CrLf`).
fn apply_line_ending(text: &str, le: LineEnding) -> String {
    match le {
        LineEnding::Lf => text.to_owned(),
        LineEnding::Cr => text.replace('\n', "\r"),
        LineEnding::CrLf => text.replace('\n', "\r\n"),
    }
}

/// Normalise `\r\n` and lone `\r` to `\n`.
fn normalise_lf(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}
