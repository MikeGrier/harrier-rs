// Copyright (c) 2026, Michael Grier

//! Encoding primitives: line-ending classification, decode-error policy,
//! and the object-safe `EncodingDetector` trait with its `chardetng`-backed
//! implementation.

use encoding_rs::Encoding;

// ── MA-1: LineEnding ──────────────────────────────────────────────────────────

/// The line-terminator convention used by a source document or output stream.
///
/// The discriminant values are stable wire constants.
/// **Changing any discriminant value is a breaking change.**
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineEnding {
    /// Unix-style: a single `\n` (U+000A).
    Lf = 0,
    /// Windows-style: `\r\n` (U+000D U+000A).
    CrLf = 1,
    /// Legacy Mac (pre-OS X): a single `\r` (U+000D).
    Cr = 2,
}

// ── MA-2: DecodeErrorPolicy ───────────────────────────────────────────────────

/// How weaver handles bytes that cannot be decoded in the detected encoding.
///
/// The variants are listed from strictest to most permissive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeErrorPolicy {
    /// Treat the first undecodable byte sequence as a hard error and return
    /// `Err` from whatever operation encountered it.  No partial output is
    /// produced.
    Fatal,

    /// Replace every undecodable byte sequence with the Unicode replacement
    /// character U+FFFD and continue.  Follows the WHATWG Encoding Standard
    /// "replacement" behaviour implemented by `encoding_rs`.
    Substitute,

    /// Scan the entire source up front and return `Err` if any byte is
    /// undecodable; if the source is clean, proceed as normal.  More
    /// expensive than `Fatal` but surfaces all errors rather than stopping
    /// at the first.
    ValidateFirst,

    /// Re-run encoding detection each time a segment that cannot be decoded
    /// is encountered and switch to the newly guessed encoding for the
    /// remainder of the document.  Intended for documents that silently mix
    /// encodings (e.g.  legacy concatenated log files).
    ContinuousDetection,
}

// ── MA-3: EncodingDetector trait ──────────────────────────────────────────────

/// Object-safe interface for heuristic byte-stream encoding detection.
///
/// Implementations accumulate evidence by consuming byte slices via `feed`
/// and then produce a best-guess `&'static Encoding` on demand via `guess`.
///
/// The object-safety constraint means the trait carries no associated types
/// and no `Self: Sized` methods.  A concrete implementation wrapping
/// `chardetng` is provided by [`ChardetngDetector`].
pub trait EncodingDetector {
    /// Feed a slice of source bytes to the detector.
    ///
    /// Set `last` to `true` on the final call to signal end-of-stream; this
    /// allows the detector to treat accumulated evidence as complete.  Calling
    /// `feed` after passing `last = true` is unspecified behaviour — callers
    /// must not do so.
    fn feed(&mut self, bytes: &[u8], last: bool);

    /// Return the best-guess encoding based on bytes fed so far.
    ///
    /// - `allow_utf8`: when `true` the detector may return `UTF_8`; when
    ///   `false` it will not (useful when the caller has already ruled out
    ///   UTF-8 by other means).
    ///
    /// This method may be called multiple times as more bytes are fed.  The
    /// result is advisory; callers should validate that the result supports
    /// both decode and encode before accepting it.
    fn guess(&self, allow_utf8: bool) -> &'static Encoding;
}

// ── MA-4: ChardetngDetector ───────────────────────────────────────────────────

/// An `EncodingDetector` implementation backed by the `chardetng` crate.
///
/// `chardetng` implements the same heuristic algorithm as Firefox's
/// character-set detector.  It operates on raw bytes and requires no BOM.
/// The `guess` result is one of the `&'static Encoding` constants defined by
/// `encoding_rs`.
pub struct ChardetngDetector {
    inner: chardetng::EncodingDetector,
}

impl ChardetngDetector {
    /// Create a new detector in its initial, unfed state.
    pub fn new() -> Self {
        ChardetngDetector {
            inner: chardetng::EncodingDetector::new(),
        }
    }
}

impl Default for ChardetngDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl EncodingDetector for ChardetngDetector {
    fn feed(&mut self, bytes: &[u8], last: bool) {
        self.inner.feed(bytes, last);
    }

    fn guess(&self, allow_utf8: bool) -> &'static Encoding {
        // `chardetng::EncodingDetector::guess` takes an optional TLD hint
        // (`None` = no hint) and an `allow_utf8` flag.
        self.inner.guess(None, allow_utf8)
    }
}

// ── MA-5: BOM detection helper ────────────────────────────────────────────────

/// Result of inspecting the leading bytes of a source for a Unicode BOM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BomResult {
    /// The encoding implied by the BOM, or `None` if no recognised BOM was
    /// found.  When `Some`, callers should skip the first `bom_len` bytes of
    /// the source before decoding content.
    pub encoding: Option<&'static Encoding>,
    /// Number of bytes consumed by the BOM (0 when `encoding` is `None`).
    pub bom_len: usize,
}

/// Inspect `probe` for a leading Unicode byte-order mark and return the
/// implied encoding and BOM length.
///
/// `probe` need not contain the full source; three bytes are sufficient to
/// identify any BOM that `encoding_rs` recognises (UTF-8, UTF-16 LE, UTF-16
/// BE).  Pass whatever prefix is conveniently available.
///
/// When no BOM is present `encoding` is `None` and `bom_len` is 0.
///
/// This function delegates to `encoding_rs::Encoding::for_bom`, which
/// implements the BOM sniffing algorithm from the WHATWG Encoding Standard.
pub fn detect_bom(probe: &[u8]) -> BomResult {
    match Encoding::for_bom(probe) {
        Some((encoding, bom_len)) => BomResult {
            encoding: Some(encoding),
            bom_len,
        },
        None => BomResult {
            encoding: None,
            bom_len: 0,
        },
    }
}

// ── MA-6: Majority-vote line-ending detector ──────────────────────────────────

/// Detect the dominant line-ending convention in a byte slice using a
/// three-step tiebreaking algorithm:
///
/// 1. **Majority**: the variant with the highest count wins outright.
/// 2. **Caller default**: when two or more variants tie, the caller's
///    `tiebreak_default` is used if it is one of the tied leaders.
/// 3. **First appearance**: if the caller default is not among the tied
///    leaders (or was not supplied), the variant that appears earliest in
///    `probe` wins.
///
/// `CRLF` is counted as a single unit; its `\r` does not also increment
/// the bare-`CR` counter.  A `\r` that is not followed by `\n` increments
/// the bare-`CR` counter.
///
/// Returns `None` when `probe` contains no line terminators of any kind,
/// in which case the caller should either use their own default or fall
/// back to `LineEnding::Lf`.
pub fn detect_line_ending(
    probe: &[u8],
    tiebreak_default: Option<LineEnding>,
) -> Option<LineEnding> {
    let mut lf_count: u64 = 0;
    let mut crlf_count: u64 = 0;
    let mut cr_count: u64 = 0;

    // First-appearance tracking: which ending was seen first, in order.
    let mut first: Option<LineEnding> = None;

    let mut i = 0usize;
    while i < probe.len() {
        match probe[i] {
            b'\r' => {
                if probe.get(i + 1) == Some(&b'\n') {
                    // CRLF — consume both bytes as one unit.
                    crlf_count += 1;
                    if first.is_none() {
                        first = Some(LineEnding::CrLf);
                    }
                    i += 2;
                } else {
                    // Bare CR.
                    cr_count += 1;
                    if first.is_none() {
                        first = Some(LineEnding::Cr);
                    }
                    i += 1;
                }
            }
            b'\n' => {
                lf_count += 1;
                if first.is_none() {
                    first = Some(LineEnding::Lf);
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    // No terminators found at all.
    if lf_count == 0 && crlf_count == 0 && cr_count == 0 {
        return None;
    }

    // Step 1: find the maximum count.
    let max = lf_count.max(crlf_count).max(cr_count);

    // Collect all variants that share the maximum count (the "leaders").
    // Using a fixed-size array avoids a heap allocation for this hot path.
    let mut leaders = [LineEnding::Lf; 3];
    let mut n_leaders = 0usize;
    for (count, ending) in [
        (lf_count, LineEnding::Lf),
        (crlf_count, LineEnding::CrLf),
        (cr_count, LineEnding::Cr),
    ] {
        if count == max {
            leaders[n_leaders] = ending;
            n_leaders += 1;
        }
    }

    if n_leaders == 1 {
        // Unambiguous majority.
        return Some(leaders[0]);
    }

    // Step 2: use the caller-supplied default if it is among the leaders.
    if let Some(default) = tiebreak_default
        && leaders[..n_leaders].contains(&default)
    {
        return Some(default);
    }

    // Step 3: first-appearance tiebreaker.
    // `first` is always `Some` here because we have at least one terminator.
    first
}

// ── MA-7: SourceConfig ────────────────────────────────────────────────────────

/// The default number of bytes read from the start of a source to probe for
/// a BOM and to run the encoding detector and line-ending majority vote.
///
/// 8 KiB is large enough to give the detector meaningful signal on most
/// real-world documents while staying well within stack or small-heap budgets.
pub const DEFAULT_PROBE_LEN: usize = 8 * 1024;

/// How a BOM found at the start of the source should be treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomPolicy {
    /// Use the BOM to determine the encoding and skip the BOM bytes from the
    /// decoded content (standard behaviour).
    Honour,

    /// Ignore any BOM present; run the encoding detector on all bytes
    /// including the BOM bytes, and include them verbatim in decoded content.
    /// Useful when the source is known to contain a spurious BOM that should
    /// be preserved as data.
    Ignore,
}

/// Configuration for opening a source document via `Source::new`.
///
/// All fields have sensible defaults via [`SourceConfig::default`].
#[derive(Debug, Clone)]
pub struct SourceConfig {
    /// An optional caller-supplied encoding hint.
    ///
    /// When `Some`, the probe step is skipped and the given encoding is used
    /// directly — unless a BOM is found and `bom_policy` is `Honour`, in
    /// which case the BOM encoding takes precedence over this hint.
    pub encoding_hint: Option<&'static Encoding>,

    /// How to handle a BOM found at the start of the source.
    pub bom_policy: BomPolicy,

    /// What to do when bytes cannot be decoded in the selected encoding.
    pub decode_error_policy: DecodeErrorPolicy,

    /// Preferred line-ending convention to use when inserting new line
    /// terminators (e.g. when `Lines` needs to emit an extra terminator
    /// during a replacement that produces more lines than the original).
    ///
    /// Also used as the tiebreaker default in majority-vote detection when
    /// the probe contains an exact tie between two or more terminator kinds.
    ///
    /// `None` means "no preference"; the detector's first-appearance
    /// tiebreaker applies instead.
    pub line_ending_default: Option<LineEnding>,

    /// Number of bytes to read from the start of the source for BOM
    /// detection, encoding detection, and line-ending majority vote.
    ///
    /// Smaller values reduce I/O but may decrease detection accuracy.
    /// Must be at least 3 (the length of a UTF-8 BOM); values below 3 are
    /// clamped to 3 at runtime.
    pub probe_len: usize,
}

impl Default for SourceConfig {
    fn default() -> Self {
        SourceConfig {
            encoding_hint: None,
            bom_policy: BomPolicy::Honour,
            decode_error_policy: DecodeErrorPolicy::Substitute,
            line_ending_default: None,
            probe_len: DEFAULT_PROBE_LEN,
        }
    }
}
