// Copyright (c) 2026, Michael Grier

//! `Chars` — the encoding-only operating mode.
//!
//! `Chars` provides the character-decoding substrate for rowan-like parser
//! crates that need to work over source files in arbitrary encodings.  It
//! does **not** build a line map or a red-green tree; it is a pure decoding
//! layer that exposes:
//!
//! - Character sync points and boundary queries (MA-19/MA-20).
//! - Forward character iteration from a known-good sync point (MA-21).
//! - Re-encoding of Unicode strings back to the source encoding (via the
//!   `Encoded` trait — MA-17).
//!
//! `Chars` is created by [`Source::as_chars`](crate::source::Source::as_chars).

use std::sync::Arc;

use encoding_rs::Encoding;
use redwing::Branch;

use crate::{encoded::Encoded, encoding::LineEnding, source::Source};

// ── MA-14: CharsError ─────────────────────────────────────────────────────────

/// Errors that can occur while operating on a [`Chars`] value.
///
/// Currently the only failure mode is an I/O error reading from the branch
/// (e.g. during a lazy sync-point index scan for DBCS encodings).  Additional
/// variants will be added in later milestones as the sync-point index (MA-18)
/// and character iteration (MA-21) are implemented.
///
/// Changing any variant or field is a breaking change.
#[derive(Debug)]
pub enum CharsError {
    /// An I/O error occurred while reading from the branch.
    ///
    /// This can arise during lazy sync-point index construction for DBCS
    /// encodings, or while decoding characters in [`Chars::chars_from`].
    Io(std::io::Error),
}

impl std::fmt::Display for CharsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CharsError::Io(e) => write!(f, "I/O error in Chars: {e}"),
        }
    }
}

impl std::error::Error for CharsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CharsError::Io(e) => Some(e),
        }
    }
}

impl From<std::io::Error> for CharsError {
    fn from(e: std::io::Error) -> Self {
        CharsError::Io(e)
    }
}

// ── MA-18: SyncKind ───────────────────────────────────────────────────────────

/// How character boundaries are detected for a given encoding.
///
/// `SyncKind` is computed once at [`Chars`] construction time and stored on
/// the struct so that every per-call boundary check avoids re-examining the
/// encoding name.
///
/// The five variants map to three detection strategies:
///
/// | Variant | Strategy |
/// |---|---|
/// | `Utf8` | O(1) — check the top two bits of the byte at the target offset |
/// | `Utf16Le` / `Utf16Be` | O(1) — check offset parity and whether the word at that offset is a low surrogate |
/// | `SingleByte` | O(1) — every offset is a boundary |
/// | `Dbcs` | lazy index — anchor bytes (LF/CR) cannot be trail bytes; decode forward from nearest anchor |
///
/// Changing any variant name or its semantic invariant is a breaking change.
/// Adding new variants is non-breaking as long as all `match` sites are kept
/// exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SyncKind {
    /// UTF-8: self-synchronising via the continuation-byte pattern (`10xxxxxx`).
    ///
    /// A byte at any offset is a boundary iff `(byte & 0xC0) != 0x80`.
    Utf8,

    /// UTF-16 little-endian: self-synchronising at even offsets.
    ///
    /// An even-offset 16-bit word is a character boundary iff it is not a
    /// low surrogate (0xDC00–0xDFFF), identified by its high byte not being
    /// in `0xDC..=0xDF`.
    Utf16Le,

    /// UTF-16 big-endian: same as `Utf16Le` but byte-swapped.
    ///
    /// A low surrogate's high byte (stored at the low-address, i.e. the first
    /// byte of the word) falls in `0xDC..=0xDF` for UTF-16BE.
    Utf16Be,

    /// Single-byte encoding: every byte offset is a character boundary.
    ///
    /// No index, no reads, and no computation are needed beyond the trivial
    /// "always true" answer.
    SingleByte,

    /// Double-byte character set (DBCS): not self-synchronising.
    ///
    /// Lead and trail bytes are indistinguishable without context.  A lazy
    /// sync-point index, seeded with anchor bytes (LF `0x0A` and CR `0x0D`)
    /// that cannot appear as trail bytes in any DBCS encoding, is used to
    /// enable O(1) *amortised* boundary detection after the first scan.
    Dbcs,
}

/// Map `encoding` to its [`SyncKind`] for boundary-detection purposes.
///
/// Called once in [`Chars::from_source`] and the result cached in
/// `Chars::sync_kind`.
///
/// Changing the `SyncKind` assigned to any encoding is a breaking change if
/// that encoding has multi-byte characters (it would cause `is_boundary` to
/// return wrong answers without a sync-point index).
pub(crate) fn sync_kind(encoding: &'static Encoding) -> SyncKind {
    match encoding.name() {
        "UTF-8" => SyncKind::Utf8,
        "UTF-16LE" => SyncKind::Utf16Le,
        "UTF-16BE" => SyncKind::Utf16Be,
        // DBCS encodings: not self-synchronising; require the lazy index.
        "Shift_JIS" | "GBK" | "gb18030" | "Big5" | "EUC-JP" | "EUC-KR" | "ISO-2022-JP" => {
            SyncKind::Dbcs
        }
        // All remaining WHATWG encodings are single-byte.
        _ => SyncKind::SingleByte,
    }
}

// ── MA-15: Chars ──────────────────────────────────────────────────────────────

/// Encoding-only view of a byte-stream branch.
///
/// `Chars` wraps a fully-resolved [`Source`] and exposes character-level
/// services without building a line map.  It is the correct choice when the
/// caller is a parser crate that manages its own tree structure and only needs
/// grouse's encoding detection, sync-point, and re-encoding services.
///
/// ## Sync-point model
///
/// For self-synchronising encodings (UTF-8, UTF-16LE, UTF-16BE), character
/// boundaries can be determined in O(1) from any byte offset without any
/// precomputed index.  For DBCS encodings (Shift-JIS, GBK, EUC-JP, EUC-KR,
/// Big5), the encoding is *not* self-synchronising: it is impossible to tell
/// from a single byte whether it is a lead byte or a trail byte without
/// knowing the context.
///
/// For DBCS encodings, `Chars` maintains a lazy **sync-point index**: a
/// sorted list of byte offsets that are known to be character boundaries,
/// seeded by LF (`0x0A`) and CR (`0x0D`) bytes, which cannot be trail bytes
/// in any DBCS encoding.  The index is populated on demand as the caller
/// navigates the branch; it is never pre-scanned up front.
///
/// The sync-point index is managed internally and is not exposed to callers.
/// Use [`nearest_sync_point`](Chars::nearest_sync_point) to query it.
///
/// ## Invariants
///
/// - Byte offset 0 is always a sync point (the branch is always aligned at
///   the first byte after the BOM).
/// - The BOM bytes are never part of the decoded character stream.
pub struct Chars {
    /// The resolved source from which this `Chars` was created.
    ///
    /// Holds the branch, encoding, line-ending policy, and BOM length.
    source: Source,

    /// Cached encoding family; computed once in [`Chars::from_source`].
    ///
    /// Drives which algorithm `nearest_sync_point` (MA-19) and `is_boundary`
    /// (MA-20) use.  Storing it avoids re-examining the encoding name on
    /// every boundary query.
    sync_kind: SyncKind,

    /// Lazy sync-point index for DBCS encodings.
    ///
    /// `None` for UTF-8, UTF-16, and single-byte encodings; those are
    /// self-synchronising and need no index.
    ///
    /// For DBCS encodings this is `Some(_)`, populated on demand during calls
    /// to [`nearest_sync_point`](Chars::nearest_sync_point) (MA-19).
    /// Wrapped in a `std::cell::RefCell` so that `nearest_sync_point` and
    /// `chars_from` can take `&self` without requiring a mutable receiver.
    sync_points: Option<std::cell::RefCell<Vec<u64>>>,
}

impl Chars {
    /// Create a `Chars` from a fully-resolved `Source`.
    ///
    /// This is called by [`Source::as_chars`](crate::source::Source::as_chars)
    /// (MA-16) and is not intended for direct use by callers.
    pub(crate) fn from_source(source: Source) -> Self {
        let sk = sync_kind(source.encoding());
        let sync_points = match sk {
            SyncKind::Dbcs => {
                // Seed with the first content byte (immediately after the BOM).
                // LF / CR cannot be trail bytes in any DBCS encoding, so they
                // are safe anchor points; `extend_sync_index_to` (MA-18) adds
                // them on demand as the caller navigates the branch.
                Some(std::cell::RefCell::new(vec![source.bom_len() as u64]))
            }
            _ => None,
        };
        Chars {
            source,
            sync_kind: sk,
            sync_points,
        }
    }

    /// The underlying byte-stream branch.
    pub fn branch(&self) -> Arc<dyn Branch> {
        self.source.branch()
    }

    /// The WHATWG encoding for this source.
    pub fn encoding(&self) -> &'static Encoding {
        self.source.encoding()
    }

    /// The dominant line-ending convention detected during source probing.
    pub fn line_ending(&self) -> LineEnding {
        self.source.line_ending()
    }

    /// The number of BOM bytes at the start of the branch to skip.
    ///
    /// The first decoded character begins at this byte offset.
    pub fn bom_len(&self) -> usize {
        self.source.bom_len()
    }

    // ── MA-19: nearest_sync_point ─────────────────────────────────────────────

    /// Return the largest byte offset ≤ `offset` that is a character boundary
    /// in the branch's encoding.
    ///
    /// This is the primary entry point for callers that want to begin decoding
    /// at or before a given byte offset.  The returned offset is always a
    /// valid starting position for [`chars_from`](Chars::chars_from) (MA-21).
    ///
    /// ## Per-encoding behaviour
    ///
    /// | Encoding family | Algorithm | I/O reads |
    /// |---|---|---|
    /// | `SingleByte` | trivially returns `offset` | none |
    /// | `Utf8` | scans backward (≤ 3 bytes) for the first non-continuation byte | ≤ 4 |
    /// | `Utf16Le` / `Utf16Be` | rounds down to nearest even offset, steps back one code unit if it is a low surrogate | ≤ 2 |
    /// | `Dbcs` | extends the lazy sync-point index to `offset`, binary-searches for the greatest entry ≤ `offset` | amortised O(new bytes) |
    ///
    /// ## Errors
    ///
    /// Returns [`CharsError::Io`] if reading from the branch fails.
    pub fn nearest_sync_point(&self, offset: u64) -> Result<u64, CharsError> {
        match self.sync_kind {
            SyncKind::SingleByte => Ok(offset),

            SyncKind::Utf8 => nearest_utf8_sync(self.branch().as_ref(), offset),

            SyncKind::Utf16Le => nearest_utf16le_sync(self.branch().as_ref(), offset),

            SyncKind::Utf16Be => nearest_utf16be_sync(self.branch().as_ref(), offset),

            SyncKind::Dbcs => {
                let branch = self.branch();
                let bom = self.bom_len() as u64;
                let cell = self
                    .sync_points
                    .as_ref()
                    .expect("DBCS always has a sync_points cell");
                let mut pts = cell.borrow_mut();
                extend_sync_index_to(&branch, bom, &mut pts, offset)?;

                // Binary search: largest entry ≤ offset.
                //
                // `partition_point` returns the first index where the predicate
                // is false (i.e. the first entry > offset).  One below that is
                // what we want.  The index is seeded with `bom_len` at
                // construction, so there is always at least one entry.
                let idx = pts.partition_point(|&p| p <= offset);
                Ok(pts[idx.max(1) - 1])
            }
        }
    }

    // ── MA-20: is_boundary ────────────────────────────────────────────────────

    /// Return `true` if `offset` is the start byte of a character in the
    /// branch's encoding.
    ///
    /// ## Per-encoding behaviour
    ///
    /// | Encoding family | Algorithm | I/O reads |
    /// |---|---|---|
    /// | `SingleByte` | always `true` | none |
    /// | `Utf8` | reads one byte; boundary iff it is not a continuation byte (`10xxxxxx`) | 1 |
    /// | `Utf16Le` | odd offset → `false`; even → reads the high byte of the word; boundary iff not a low surrogate | ≤ 1 |
    /// | `Utf16Be` | odd offset → `false`; even → reads the first byte of the word; boundary iff not a low surrogate | ≤ 1 |
    /// | `Dbcs` | extends the lazy sync-point index to `offset`, then checks whether `offset` is in the index; only anchor bytes (LF/CR) are recognised boundaries without forward decoding | amortised O(new bytes) |
    ///
    /// ## DBCS boundary semantics
    ///
    /// For DBCS encodings the sync-point index records only LF (`0x0A`) and
    /// CR (`0x0D`) bytes — the only offsets that are provably character
    /// boundaries without decoding forward from a known-good sync point.
    /// Callers that need to test arbitrary DBCS offsets should use
    /// [`chars_from`](Chars::chars_from) (MA-21) starting from the
    /// [`nearest_sync_point`](Chars::nearest_sync_point) and walk forward.
    ///
    /// ## Errors
    ///
    /// Returns [`CharsError::Io`] if reading from the branch fails.
    pub fn is_boundary(&self, offset: u64) -> Result<bool, CharsError> {
        match self.sync_kind {
            SyncKind::SingleByte => Ok(true),

            SyncKind::Utf8 => {
                let byte = self.branch().read_byte(offset)?;
                Ok(is_utf8_boundary_byte(byte))
            }

            SyncKind::Utf16Le => {
                if !offset.is_multiple_of(2) {
                    return Ok(false);
                }
                // Need room for the full 16-bit word; if the branch is too
                // short treat the offset as a boundary (end of stream).
                if offset + 1 >= self.branch().byte_len() {
                    return Ok(true);
                }
                let high_byte = self.branch().read_byte(offset + 1)?;
                Ok(is_utf16le_boundary(offset, high_byte))
            }

            SyncKind::Utf16Be => {
                if !offset.is_multiple_of(2) {
                    return Ok(false);
                }
                if offset >= self.branch().byte_len() {
                    return Ok(true);
                }
                let first_byte = self.branch().read_byte(offset)?;
                Ok(is_utf16be_boundary(offset, first_byte))
            }

            SyncKind::Dbcs => {
                // Delegate to nearest_sync_point: if the nearest sync point
                // at-or-before `offset` is exactly `offset`, then `offset` is
                // a known anchor boundary.
                Ok(self.nearest_sync_point(offset)? == offset)
            }
        }
    }

    // ── MA-21: chars_from ─────────────────────────────────────────────────────

    /// Return an iterator that yields the decoded characters in the branch
    /// starting at `offset`.
    ///
    /// `offset` **must** be a character boundary.  The correct way to ensure
    /// this is to pass a value returned by
    /// [`nearest_sync_point`](Chars::nearest_sync_point) (MA-19).  Passing an
    /// offset that falls inside a multi-byte sequence produces garbled output
    /// (no panic); passing an offset past the end of the branch produces an
    /// empty iterator.
    ///
    /// I/O errors encountered during iteration are silently treated as
    /// end-of-stream.  If exact error reporting is needed, use
    /// [`nearest_sync_point`] to validate the starting offset and read the
    /// branch directly.
    pub fn chars_from(&self, offset: u64) -> CharsIter {
        CharsIter {
            branch: self.branch(),
            decoder: self.encoding().new_decoder_without_bom_handling(),
            pos: offset,
            pending: String::new(),
            pending_pos: 0,
            eof_fed: false,
        }
    }
}

// ── MA-18: Internal boundary-detection helpers ───────────────────────────────

/// Return `true` if `byte` is the first byte of a UTF-8 character sequence.
///
/// UTF-8 continuation bytes have the bit pattern `10xxxxxx`.  Any byte whose
/// top two bits are **not** `10` is a character-boundary byte — it is either
/// an ASCII byte (`0xxxxxxx`) or a multi-byte lead byte (`11xxxxxx`).
///
/// This is O(1) and requires no precomputed index.  The byte at the target
/// offset must already be in hand (one read from the branch).
#[inline]
pub(crate) fn is_utf8_boundary_byte(byte: u8) -> bool {
    (byte & 0xC0) != 0x80
}

/// Return `true` if `(offset, high_byte)` is the start of a UTF-16LE code unit.
///
/// In UTF-16LE a character boundary occurs at an even byte offset whose 16-bit
/// word is **not** a low surrogate.  Low surrogates occupy code points
/// U+DC00–U+DFFF; in little-endian layout the high byte (at `offset + 1`) lies
/// in `0xDC..=0xDF`.
///
/// `high_byte` is the byte at `offset + 1` (i.e. the most-significant byte of
/// the 16-bit word).
///
/// Odd offsets always return `false` without reading any bytes.
#[inline]
pub(crate) fn is_utf16le_boundary(offset: u64, high_byte: u8) -> bool {
    offset.is_multiple_of(2) && !(0xDC..=0xDF).contains(&high_byte)
}

/// Return `true` if `(offset, low_byte)` is the start of a UTF-16BE code unit.
///
/// In UTF-16BE a low surrogate's most-significant byte is stored first (at
/// `offset`), so `low_byte` is the byte at `offset` itself, and a low
/// surrogate is identified by that byte being in `0xDC..=0xDF`.
///
/// Odd offsets always return `false` without reading any bytes.
#[inline]
pub(crate) fn is_utf16be_boundary(offset: u64, first_byte: u8) -> bool {
    offset.is_multiple_of(2) && !(0xDC..=0xDF).contains(&first_byte)
}

/// Extend the DBCS sync-point index until it covers at least `upto`.
///
/// Reads the branch in ≤ 4096-byte chunks, starting one past the last entry
/// in `sync_points` (or from `bom_len` if the index is empty), and appends
/// the offset of every LF (`0x0A`) and CR (`0x0D`) byte found.
///
/// ## Why LF / CR are safe anchor bytes
///
/// In every DBCS encoding supported by the WHATWG Encoding Standard (Shift-JIS,
/// GBK, gb18030, Big5, EUC-JP, EUC-KR, ISO-2022-JP) the bytes `0x0A` and
/// `0x0D` **cannot** appear as the trail byte of a multi-byte sequence.
/// ISO-2022-JP additionally mandates that the encoding be in ASCII mode at
/// every line boundary, so physical `0x0A`/`0x0D` bytes there are always the
/// complete ASCII LF/CR character.  Each anchor byte therefore marks a known
/// character boundary from which forward decoding is safe.
///
/// ## Post-condition
///
/// On success `sync_points` is sorted in ascending order.  Its last element
/// may be less than `upto` if no anchor bytes were found in the requested
/// range (the index is still extended as far as the scan reached).
///
/// ## Errors
///
/// Returns `Err` if reading from `branch` fails.
pub(crate) fn extend_sync_index_to(
    branch: &Arc<dyn Branch>,
    bom_len: u64,
    sync_points: &mut Vec<u64>,
    upto: u64,
) -> std::io::Result<()> {
    // Start one past the last known sync point so we never rescan covered
    // ground.  If the index is empty we start from the first content byte.
    let scan_start = sync_points
        .last()
        .copied()
        .unwrap_or(bom_len)
        .saturating_add(1);

    if scan_start > upto {
        return Ok(()); // index already covers the requested range
    }

    const CHUNK: usize = 4096;
    let mut buf = [0u8; CHUNK];
    let mut pos = scan_start;

    while pos <= upto {
        let want = (upto - pos + 1).min(CHUNK as u64) as usize;
        let n = branch.read_at(pos, &mut buf[..want])?;
        if n == 0 {
            break; // reached end of branch
        }
        for (i, &byte) in buf[..n].iter().enumerate() {
            if byte == 0x0A || byte == 0x0D {
                sync_points.push(pos + i as u64);
            }
        }
        pos += n as u64;
    }

    Ok(())
}

// ── MA-19: nearest-sync-point scan helpers ────────────────────────────────────

/// Walk backward from `offset` in a UTF-8 branch until landing on a
/// non-continuation byte, which is the start of a UTF-8 character.
///
/// UTF-8 continuation bytes are `10xxxxxx`; at most three appear consecutively
/// (in a 4-byte sequence), so this loop always terminates in ≤ 4 reads.
///
/// Returns `Ok(0)` if the backward walk reaches byte 0 before finding a
/// leading byte (which can happen only in malformed UTF-8).
fn nearest_utf8_sync(branch: &dyn Branch, offset: u64) -> Result<u64, CharsError> {
    let mut pos = offset;
    loop {
        let byte = branch.read_byte(pos)?;
        if is_utf8_boundary_byte(byte) {
            return Ok(pos);
        }
        if pos == 0 {
            // Malformed stream: treat byte 0 as a boundary regardless.
            return Ok(0);
        }
        pos -= 1;
    }
}

/// Walk backward from `offset` in a UTF-16LE branch to find the start of the
/// code point that contains (or begins at) `offset`.
///
/// Steps:
/// 1. Round down to the nearest even byte offset (start of a code unit).
/// 2. If that code unit is a low surrogate (`high_byte` in `0xDC..=0xDF`),
///    step back one more code unit to land on the high surrogate instead.
///
/// A single backward step is possible because a low surrogate never follows
/// another low surrogate in valid UTF-16.
fn nearest_utf16le_sync(branch: &dyn Branch, offset: u64) -> Result<u64, CharsError> {
    // Round down to the start of the enclosing code unit.
    let mut pos = offset & !1u64;
    loop {
        // Guard: if there is not room for a full 16-bit word, we cannot read
        // the high byte, so treat `pos` as a boundary (edge case at end of
        // branch).
        if pos + 1 >= branch.byte_len() {
            return Ok(pos);
        }
        let high_byte = branch.read_byte(pos + 1)?;
        if is_utf16le_boundary(pos, high_byte) {
            return Ok(pos);
        }
        // Low surrogate: its matching high surrogate is two bytes earlier.
        if pos < 2 {
            return Ok(0); // malformed: low surrogate at start; treat as boundary
        }
        pos -= 2;
    }
}

/// Walk backward from `offset` in a UTF-16BE branch to find the start of the
/// code point that contains (or begins at) `offset`.
///
/// Same as [`nearest_utf16le_sync`] but the discriminating byte is the first
/// byte of the code unit (stored at `pos`) rather than the second.
fn nearest_utf16be_sync(branch: &dyn Branch, offset: u64) -> Result<u64, CharsError> {
    let mut pos = offset & !1u64;
    loop {
        if pos >= branch.byte_len() {
            return Ok(pos);
        }
        let first_byte = branch.read_byte(pos)?;
        if is_utf16be_boundary(pos, first_byte) {
            return Ok(pos);
        }
        if pos < 2 {
            return Ok(0); // malformed: low surrogate at start
        }
        pos -= 2;
    }
}

// ── MA-21: CharsIter ────────────────────────────────────────────────────────

/// Character iterator produced by [`Chars::chars_from`].
///
/// Decodes the branch byte-stream from a given offset using the encoding
/// detected at [`Source`]-construction time.  Reads the branch in 4096-byte
/// chunks and feeds them through an `encoding_rs` streaming decoder.
///
/// I/O errors are treated as end-of-stream: the iterator stops cleanly rather
/// than panicking.
///
/// Created by [`Chars::chars_from`]; not intended for direct construction.
pub struct CharsIter {
    branch: Arc<dyn Branch>,
    decoder: encoding_rs::Decoder,
    /// Current read position in the branch (advances as bytes are consumed).
    pos: u64,
    /// Decoded characters not yet yielded, stored as a UTF-8 `String`.
    pending: String,
    /// Byte index within `pending` of the next character to yield.
    pending_pos: usize,
    /// `true` once `last=true` has been passed to the decoder (EOF reached).
    eof_fed: bool,
}

impl Iterator for CharsIter {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        const CHUNK: usize = 4096;

        loop {
            // Yield from the previously decoded but not-yet-returned buffer.
            if self.pending_pos < self.pending.len() {
                let s = &self.pending[self.pending_pos..];
                let ch = s.chars().next().unwrap();
                self.pending_pos += ch.len_utf8();
                return Some(ch);
            }

            if self.eof_fed {
                return None;
            }

            // Refill: read the next chunk from the branch.
            self.pending.clear();
            self.pending_pos = 0;

            let mut raw = [0u8; CHUNK];
            let n = match self.branch.read_at(self.pos, &mut raw) {
                Ok(n) => n,
                Err(_) => return None, // I/O error → treat as EOF
            };

            let is_last = n == 0;

            // Allocate output space.
            //
            // `max_utf8_buffer_length(n)` gives a tight upper bound for all
            // encodings when the output buffer will not fill mid-decode.
            // A minimum of 32 bytes is used for the EOF flush pass (n == 0)
            // where stateful encodings (e.g. ISO-2022-JP) may still need to
            // emit replacement characters for any pending escape sequence.
            let max_out = self
                .decoder
                .max_utf8_buffer_length(n)
                .unwrap_or_else(|| n.saturating_mul(4).saturating_add(4))
                .max(32);
            let mut out = vec![0u8; max_out];

            let (_result, bytes_read, bytes_written, _had_errors) =
                self.decoder.decode_to_utf8(&raw[..n], &mut out, is_last);

            // Advance past the bytes the decoder consumed.  `bytes_read` may
            // be less than `n` if the decoder held back a partial sequence;
            // next iteration will re-read those unprocessed bytes.
            self.pos += bytes_read as u64;

            // encoding_rs guarantees its output is valid UTF-8.
            let decoded = std::str::from_utf8(&out[..bytes_written])
                .expect("encoding_rs output is valid UTF-8");
            self.pending.push_str(decoded);

            if is_last {
                self.eof_fed = true;
            }
        }
    }
}

// ── MA-17: Encoded for Chars ──────────────────────────────────────────────────

impl Encoded for Chars {
    fn branch(&self) -> Arc<dyn Branch> {
        self.branch()
    }

    fn encoding(&self) -> &'static Encoding {
        self.encoding()
    }

    fn line_ending(&self) -> LineEnding {
        self.line_ending()
    }
}
