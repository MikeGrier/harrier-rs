# weaver

A Rust crate providing a line-map and character-encoding–aware red-green tree for structured,
lossless, incrementally-editable text, built on top of the [`redwing`](../redwing/README.md) crate.

## Overview

`weaver` is the text layer that sits directly above `redwing`.  Where `redwing` operates purely at
the level of raw bytes — tracking deltas, managing piece tables, and providing non-destructive change
management — `weaver` introduces the concepts that are meaningful for *text*:

- **Lines.** A structured line map over the byte buffer, allowing efficient translation between
  byte offsets and `(line, column)` coordinates regardless of the line-ending convention in use
  (`LF`, `CRLF`, or legacy `CR`).
- **Character encoding.** Transparent decoding and re-encoding of byte regions using a declared
  source encoding (UTF-8, UTF-16LE, UTF-16BE, Latin-1, and others via `encoding_rs`).  Offset
  arithmetic inside `weaver` is always performed in bytes; decoded character views are derived
  on demand and never alter the underlying byte representation.
- **A red-green tree.** A two-level tree structure in the spirit of the
  [`rowan`](https://github.com/rust-analyzer/rowan) crate, adapted to be encoding-agnostic.  The
  *green* level is a shared, cheaply-cloned structural description of the text.  The *red* level
  is a positioned, navigable cursor over that structure, backed by a `redwing` branch so that edits
  remain non-destructive and reversible.

## Relationship to `redwing`

`redwing` provides the byte-level change-management primitive: a `Thicket` (base branch) and one or
more `DerivedBranch` objects that each carry an independent set of deltas against the base.  It has
no knowledge of encoding, lines, or characters.

`weaver` wraps a `redwing` branch and adds the text-specific layer on top:

```
┌─────────────────────────────────────────────┐
│                   weaver                    │  ← line map, encoding, red-green tree
├─────────────────────────────────────────────┤
│                   redwing                   │  ← byte deltas, piece table, change management
├─────────────────────────────────────────────┤
│            raw byte source (file,           │
│            memory-mapped, or buffer)        │
└─────────────────────────────────────────────┘
```

When a text edit is made through `weaver`, it computes the affected byte range (accounting for the
source encoding and any multi-byte characters), constructs a replacement byte sequence in the same
encoding, and hands that byte-level replacement to the underlying `redwing` branch as a delta.  The
original bytes are never mutated.

## Relationship to `rowan`

[`rowan`](https://github.com/rust-analyzer/rowan) is an excellent, widely-used crate for lossless,
incrementally-editable syntax trees.  Its fundamental limitation for this project is its
unconditional assumption that the source is UTF-8.  Every token, every node, every text offset in
`rowan` is a UTF-8 byte index.  Working with UTF-16LE Windows source files, Latin-1 legacy
documents, or other encodings requires a lossy up-front transcode — which changes byte offsets and
makes round-tripping back to the original encoding fragile.

`weaver` takes a different approach:

| Concept in `rowan`               | Concept in `weaver`                                      |
|----------------------------------|----------------------------------------------------------|
| Source text (UTF-8 only)         | Byte buffer with declared encoding (any `encoding_rs` encoding) |
| UTF-8 text offset                | Byte offset (primary); decoded char offset derived on demand |
| Green tree (shared, cheaply cloned) | Green tree (same semantics)                           |
| Red tree (owned cursor)          | Red tree backed by a `redwing` `DerivedBranch`           |
| Incremental reparse              | Delta merge + tree revalidation                          |
| `SyntaxNode` / `SyntaxToken`     | `TextNode` / `TextToken` (encoding-aware)                |
| Edit API (`SyntaxEditor`)        | Edit API over decoded text, emits byte deltas to `redwing` |

`weaver` does not aim to be a drop-in replacement for `rowan`; the API will differ where necessary
to honour source encodings and to expose the richer change-management model that `redwing` provides.

## Input Model

`weaver` takes its byte data from a `redwing` `Branch` — specifically an `Arc<dyn Branch>`.  The
choice of `Branch` rather than `Thicket` is deliberate:

- A `Thicket` is an **owner** of the first writable branch and the root of a fork tree.  `weaver`
  does not need to own the thicket; it only needs read and (optionally) write access to one branch
  within it.
- An `Arc<dyn Branch>` is the natural handle for that: it allows multiple consumers to share the
  same branch, and the interior-mutability of `DerivedBranch` means writes from `weaver` are
  recorded as deltas without requiring `weaver` to hold exclusive access.

In addition to the branch, `weaver` accepts an optional **byte range** `[start, end)` within the
branch.  This allows `weaver` to treat an embedded text region inside a larger binary file as its
working domain, without requiring the caller to slice or copy the data.  When no range is
supplied, the entire branch is the working domain.

### No Materialisation

`weaver` **must not require the full byte content to be resident in memory at once.**  The
`Branch` trait provides `read_at(offset, buf)` and `as_reader()` — both are lazy, demand-driven
interfaces that read only as many bytes as the caller requests.  `weaver` uses these exclusively
for scanning:

- The **line map** is built lazily and incrementally, never requiring a full up-front scan.  It
  stores only line-start byte offsets — O(lines) memory, not O(bytes).
- The **red-green tree** stores byte spans (start offset + length), not inline byte content.
  Actual bytes are fetched via `read_at` only when a text value is decoded and returned to the
  caller.
- **Search** works as a streaming pass over `as_reader()`, maintaining a sliding window whose
  maximum size is bounded by a compile-time constant (target: ≤ 256 KiB).
- **Sed-mode output** is pushed toward a caller-supplied `Write` sink as deltas are resolved,
  rather than accumulated into a single output buffer.  The internal holding buffer is bounded;
  the target is no more than 128–256 KiB in flight at any moment between the source scan and
  the output sink.

This means a multi-gigabyte log file can be scanned, patched, and streamed to an output file
while holding only a small, bounded amount of memory.

## Use Cases

`weaver` is designed to serve two distinct but closely related consumers.  The two modes differ
enough in their data-structure requirements that they are treated as separate operating modes
rather than a single unified API:

### 1. Sed mode — streaming search/replace engine

A sed/grep-style tool needs:

- **Streaming scan.** Process terabyte-scale inputs without loading the file into memory.  The
  source is consumed forward-only via `as_reader()`; no random access into earlier regions is
  required.
- **Encoding transparency.** Match the logical text `"café"` in a Latin-1 file, a UTF-8 file,
  and a UTF-16LE file with the same search query, regardless of how many bytes each character
  occupies.
- **Line-ending transparency.** Treat `\n`, `\r\n`, and `\r` as equivalent line terminators
  when matching line-anchored patterns, without changing bytes that are not part of a match.
- **Push output.** Resolved output bytes are pushed to a caller-supplied `Write` sink as the
  scan progresses.  At no point is the full output buffered in memory; the internal pipeline
  holds at most a bounded sliding window (target: 128–256 KiB) between source and sink.
- **Precise, lossless replacement.** Replacement text is re-encoded in the source encoding and
  spliced in as a `redwing` byte delta.  Bytes outside any match are forwarded unchanged.

Sed mode does **not** build a line map or a red-green tree.  It is a pure streaming pipeline:
bytes in, deltas resolved, bytes out.

### 2. Editor mode — line-map and red-green tree

An editor needs:

- **Random-access by position.** Given a cursor at `(line 42, column 7)` in a UTF-16LE file,
  resolve the exact byte offset in the branch instantly.
- **Lazy line-map construction.** The line map must not require a full up-front scan of the
  file.  It is built incrementally as the editor navigates: regions that have never been
  visited are not yet indexed.  The map is a multi-level segmented structure so that individual
  segments can be populated on demand and replaced after edits without touching the whole map.
- **Incremental update after edits.** After a single-line change, only the affected segment(s)
  of the line map are invalidated and recomputed; lines above and below the edit are untouched.
- **Structural navigation.** Walk the green/red tree to find tokens, annotate nodes, and produce
  diagnostics with accurate source positions.
- **Non-destructive editing.** Every change is a `redwing` delta; undo discards the branch,
  redo re-applies a saved change set.

Editor mode builds the line map and optionally the red-green tree.  The line map is an
opt-in feature; a consumer that has no need of `(line, column)` coordinates (e.g. a
binary-patch tool) can skip it.

### 3. Encoding-only mode — arbitrary-encoding parser substrate

A rowan-compatible parser crate that wants to operate over source files in arbitrary encodings
needs none of weaver's line abstraction — it manages its own tree structure — but it does need:

- **Encoding detection.** Determine the source encoding automatically from the probe buffer,
  with all the same BOM and chardetng machinery as the other modes.
- **Character boundary sync points.** For variable-width encodings (DBCS: Shift-JIS, GBK,
  EUC-JP, EUC-KR, Big5; and UTF-8; UTF-16) arbitrary byte offsets may not be character
  boundaries.  The parser crate needs a way to find the nearest known-good character boundary
  at or before any given offset so it can begin decoding forward from a safe position.
- **Forward character iteration from a sync point.** Decode characters in sequence from a
  known-good offset, yielding `(start_offset, char, end_offset)` triples in branch coordinates.
- **Re-encoding for insertion.** When the parser crate wants to insert or replace text, it
  has Unicode strings; weaver must re-encode them to the source encoding's byte sequences.

Encoding-only mode provides exactly these four services and nothing else.  No line map is built,
no line abstraction is presented, and the caller is responsible for all structural interpretation
of the decoded character stream.  The rowan-like crate works entirely in Unicode `char` values
and source byte ranges (`BranchOffset`); it never touches raw encoding bytes.

### Mode Selection — Type-System Encoding

**Resolved design decision.** The choice of operating mode is encoded in the Rust type system
through three distinct types produced by a shared builder, rather than through a Cargo feature
flag or a runtime boolean.  The consequences:

- Operations that only make sense in a given mode are methods on that mode's type only.  A
  `Lines` value cannot call editor methods; the error is a compile-time type error, not a
  runtime panic.
- A binary that never constructs the `Buffer` type will have the line map and segment tree
  eliminated by the dead-code stripper.  The protection is therefore equivalent to a feature
  flag for any given binary, without the ecosystem friction of requiring downstream callers to
  declare `features = [...]`.
- A Cargo feature flag remains available as a future option if embedded or
  size-constrained targets become a requirement, but it is not the primary mechanism.

**Resolved type names.**

| Type | Mode | Purpose |
|---|---|---|
| `Chars` | Encoding-only | Decoded character stream over arbitrary encodings; substrate for rowan-like parser crates. |
| `Lines` | Streaming | Forward-only line-oriented stream; the sed/grep use case. |
| `Buffer` | Editor | Holds the lazy segmented line map and red-green tree; the editor buffer use case. |

The shared builder that produces all three types is `Source`.  The normalised-view type returned
by `Source::normalised_view()` is `View`, coupling the materialised `Vec<u8>` buffer with the
offset map and the `view.apply()` method.

**Shared trait — Resolved.** `Chars`, `Lines`, and `Buffer` all share the encoding layer:
they all detect the source encoding, hold an `Arc<dyn Branch>` reference, and expose
`encode()`.  A common trait — `Encoded` — captures this shared surface so that generic code
can accept any of the three types without caring which mode is in use.

**Error types — Resolved.** Each type exposes its own error type specialised to the failures
that can occur in that mode.  There is no single crate-wide `Error` enum.  Layers that are
shared (e.g. the encoding detection pipeline) define their own error types that the per-mode
errors wrap or re-export as variants.

## Core Concepts

### Line Ending Abstraction

**Design intent.** `weaver`'s client-facing abstraction presents text as a sequence of *lines* —
the line-ending bytes are not visible to clients.  A client iterates lines, reads line content,
and writes line content without ever seeing or producing `\n`, `\r\n`, or `\r` bytes directly.
The line endings live below the abstraction boundary in the `redwing` byte layer, where they are
preserved losslessly in the original form.

This is the same separation that most high-level text editors apply internally: the user types
characters, not control codes, and the editor handles the encoding of line boundaries into its
storage format.

**Compatibility with lossless storage.** The two goals are not in conflict as long as the
abstraction boundary is observed consistently:

- *Reading* a line: the client receives the line's decoded character content, without the
  trailing line-ending bytes.  The bytes exist in the branch; they are simply not surfaced.
- *Writing* into the middle of an existing line: no line boundary is crossed, so no line-ending
  bytes need to be produced.  The replacement is re-encoded and spliced directly.
- *Inserting a new logical line break*: a line-ending byte sequence must be chosen and inserted
  into the branch.  This is the point where a policy is required (see *Line Ending Policy* below).
- *Generating an output character stream*: the caller may want the output line endings to differ
  from the source (e.g. normalise CRLF → LF).  This is also a policy decision.

**Open design questions.**  The following points are not yet resolved and are recorded here for
the detailed design phase:

1. **How is a new line break encoded? — Resolved.**  The byte sequence used when inserting a
   logical line break is determined at open time and stored as a single file-level property,
   `insert_line_ending`.  The value is one of the three sequences git recognises as meaningful:

   | Variant | Bytes | Notes |
   |---|---|---|
   | `LF` | `0x0A` | Unix default; what git stores internally |
   | `CRLF` | `0x0D 0x0A` | Windows default |
   | `CR` | `0x0D` | Legacy Classic Mac OS; rare in new files |

   **Detection — Resolved.** `weaver` scans the probe buffer (the same up-to-8 KiB block
   already read for encoding detection — no second probe) and counts the occurrences of each
   terminator kind.  The winner is selected by the following rule in order:

   1. **Majority.** The terminator kind with the highest count wins.
   2. **Tiebreak by caller default.** If two or more kinds share the highest count and one of
      them matches the caller-supplied default (see *Override* below), that kind wins.
   3. **Tiebreak by first appearance.** If two or more kinds share the highest count and none
      matches the caller default, the one that appeared first in the probe buffer wins.

   If the probe buffer contains no line ending at all (file shorter than 8 KiB with no newline,
   or the first line extends beyond the probe), the fallback is `LF`.

   **Override.** The caller may set `insert_line_ending` explicitly in the initialisation
   options, bypassing detection entirely.  This is the correct choice when the tool has a
   policy (e.g. "always write CRLF for Windows targets") that should not be influenced by
   what happens to be in the file.  The caller default also participates in tiebreaking when
   detection is not overridden.

2. **Mixed line endings — Resolved.**  If a file has mixed endings (`\n` on some lines,
   `\r\n` on others), the logical abstraction still works correctly — each line boundary is
   identified regardless of which byte sequence terminates it.  The detected `insert_line_ending`
   is whichever kind appeared most often in the probe buffer, determined by the majority-vote
   rule above.  This is a better heuristic than first-seen for mixed files and correctly
   identifies the dominant convention in the probe window.  Callers that need deterministic
   behaviour regardless of file content can always override explicitly.

3. **Output line ending policy — Resolved.**  The default behaviour in all modes is to
   preserve each line's original terminator exactly as it appeared in the source.  No uniform
   conversion is applied unless the caller explicitly requests one.  When a conversion directive
   is supplied, all terminators in the output are replaced with the specified `LineEnding`
   uniformly.  This is a general output policy, not specific to any one mode.

   **Implementation note.** This maps directly onto the `DenormaliseWriter` iterator `I`: in
   the default preserve case, `I` yields the original terminators from the terminator log or
   line map record; in the conversion case, `I` is replaced with an iterator that always yields
   the target `LineEnding`.  The same combinator handles both cases with no structural change.

   **The common fast path.** UTF-8 source with terminator preservation is by far the most
   frequent case.  In this configuration there is no transcoding work and no line-ending byte
   substitution — the pipeline reduces to a direct passthrough of the source bytes for
   unmatched regions.  This path must incur no extra cost; the generality of supporting other
   encodings and conversion directives must not impose overhead on it.

4. **What the client sees for line iterators — Resolved.**  A line iterator yields decoded
   character strings without trailing line-ending characters.  The line map exposes the
   terminator kind (`LF`, `CRLF`, `CR`, or `EOF` for the last line) as queryable metadata per
   line.  This is consistent with the `DenormaliseWriter` requirement — the line map must store
   terminator kinds internally anyway — and makes the information available to callers that need
   to report or reason about per-line endings without breaking the primary abstraction (clients
   never *receive* terminator bytes in line content; they query the kind separately if they care).

5. **Inserting line-break characters through the content API — Resolved.**  If a caller
   attempts to insert or replace text within a line using the line-content API, and the
   replacement string contains any character or byte sequence that `weaver` would recognise as
   a line boundary during scanning, the operation returns `Err`.  It is not silently split into
   multiple lines.  Structural operations — splitting a line at a position, appending a new
   empty line, inserting a line after another — are distinct methods that go through the
   `insert_line_ending` policy explicitly.  Allowing line-boundary characters through the
   content API would create new line boundaries invisibly, corrupting the line map's index
   without it being aware.

   **The prohibition is self-referential by design.**  The set of sequences that are illegal
   to insert through the content API is exactly the set of sequences that `weaver` recognises
   as line terminators.  There is no separate list to maintain; the two are defined together.
   If `weaver`'s recognised line-ending set ever changes, the content-API prohibition changes
   with it automatically and consistently.

### Multi-Line Regex and the Normalised Logical View

`weaver` supports multi-line regular expressions by providing a **normalised view** of the
branch content.  Because `weaver` hides line terminators from clients (see *Line Ending
Abstraction* above), it must also hide them from the regex engine — a pattern author cannot
write a portable multi-line pattern if the raw terminator bytes are visible, since `foo\nbar`
would fail against a CRLF file and never match a CR-only file.

**weaver is not a regex runner.** The caller owns the regex engine, constructs the pattern, and
drives the match loop.  weaver's role is to provide the contiguous normalised buffer the regex
engine requires and to translate match positions back to branch coordinates when the caller
wants to apply a replacement.

**`NormalisedView` — the central type.** A `NormalisedView` is obtained from the source and
encapsulates everything needed for a match-and-replace loop:

```rust
let mut view = source.normalised_view(..)?;

for mat in pattern.find_iter(view.as_bytes()) {
    let replacement = compose_replacement(&mat, view.as_bytes());
    view.apply(mat.range(), replacement.as_bytes())?;
}
```

`NormalisedView` owns:

- The materialised `Vec<u8>` — every line terminator in the branch, regardless of whether it
  is `LF`, `CRLF`, or `CR`, is represented as a single `\n`.  Pattern authors always write
  `\n`.  Capture group content extracted from the buffer also contains `\n`, never `\r\n` or
  `\r`.
- The **offset map** — a compact sorted list of `(normalised_position, cumulative_drift)`
  pairs, one per CRLF terminator.  For LF and CR files the map is empty (offsets are
  identical); for CRLF files it represents the cumulative byte expansion.

`view.as_bytes() -> &[u8]` — hands the normalised buffer to the regex engine.

`view.apply(normalised_range: Range<usize>, replacement: &[u8]) -> Result<()>` — the single
method for writing a replacement:
1. Translates `normalised_range` to branch coordinates via a binary search on the offset map.
2. Determines the original terminators `t₁ … tₙ` for the matched span (N = count of `\n` in
   that range of the normalised buffer).
3. Denormalises the replacement bytes: each `\n` in `replacement` is converted to the
   appropriate terminator byte sequence according to the terminator preservation rule (see
   below), enforcing the content API prohibition structurally.
4. Hands the resulting byte splice to `redwing`.

**Borrow consequence.** While a `NormalisedView` exists, the source is mutably borrowed.  No
second view can be materialised and no other branch access is possible while the view is live.
Rust's borrow checker enforces this: the pair (materialised buffer, offset map) must remain
coherent with the branch for the duration of the loop.

**Redwing property that makes the loop safe.** `redwing` deltas are always expressed against
the *original* branch coordinates, not adjusted coordinates.  Applying replacement 1 does not
shift the branch coordinates of replacement 2.  The offset map computed at materialisation time
remains valid for every `view.apply()` call in the loop — no recomputation between iterations.

**Memory implication.** The normalised buffer must be contiguous.  For multi-line regex over a
range of the branch, that range (decoded and normalised) must fit in memory.  For ranges within
the 256 MiB ceiling this is supported.  For larger ranges, `normalised_view` returns an error
at materialisation time.  Single-line patterns do not require a full materialisation; weaver can
provide single-line content line by line without buffering the whole file (see the line iterator
API).

**Replacement denormalisation — terminator preservation rule.** The replacement bytes passed to
`view.apply()` are in normalised form: line breaks are written as `\n`.  Before the bytes enter
the branch, `view.apply()` denormalises them.  Let N be the number of line terminators in the
matched span (`t₁, t₂, … tₙ`) and M be the number of `\n` characters in the replacement:

| Relationship | Denormalisation rule |
|---|---|
| M == N | Substitute `tᵢ` for the *i*-th `\n`. Every original terminator is preserved exactly. |
| M < N | Substitute `tᵢ` for the *i*-th `\n`, using only the first M. The rest belonged to the matched span and are discarded with it. |
| M > N | Substitute `tᵢ` for the first N `\n`s; use `insert_line_ending` for positions N+1 through M. |

The M == 0 case (replacement contains no `\n`) is the degenerate M < N case: all original
terminators are discarded and the replacement is a single-line span.  No special handling.

**`DenormaliseWriter` — allocation-free enforcement.** The denormalisation in `view.apply()` is
performed by a generic, fully statically-dispatched adapter:

```
DenormaliseWriter<W: Write, I: Iterator<Item = LineEnding>>
```

The `\n` characters in the replacement are structural separators — "next line begins here" —
not literal bytes.  `DenormaliseWriter` converts each to the correct terminator byte sequence,
ensuring that no raw line-boundary bytes ever enter the branch.  This is structural enforcement
of the content API prohibition (see *Line Ending Abstraction — point 5* above), not a runtime
check.  In editor mode the same combinator is reused with `I` sourced from the line map's
per-line terminator records.

### Line Map (editor mode only)

The line map is an opt-in index available in editor mode.  Given any byte offset it answers:
*which line is this on, and what is the byte offset of that line's first byte?*  Given a
`(line, column)` pair (where column is expressed in decoded characters) it answers: *what is the
corresponding byte offset?*

The line map is encoding-aware: a "column" in a UTF-16LE file counts the appropriate unit for that
encoding (code units for UTF-16, bytes for single-byte encodings).  Line-ending detection is
configurable; by default all three conventions (`LF`, `CRLF`, `CR`) are recognised.

**Lazy, segmented construction.** The map is not built up-front.  Instead it is a multi-level
segmented tree — think of it as a sparse array of fixed-size *segment blocks*, each covering a
constant number of lines.  A segment is populated only when a query touches its range for the first
time.  When an edit invalidates a region, only the affected segment(s) are marked dirty and
recomputed; all other segments remain valid.  This means:

- Opening a 1-million-line file and immediately placing the cursor on line 1 costs one segment
  scan, not a full-file scan.
- Editing line 500 invalidates at most a handful of segments around the change point.
- The overall memory cost is O(lines actually visited), not O(total lines in the file).

**Line count — progressive accuracy.** Because the map is built lazily, the total line count is
not known until the entire file has been scanned.  weaver exposes line count as a value paired
with an exactness flag:

```rust
struct LineCount {
    value: usize,
    exact: bool,
}
```

Before the whole file is scanned, `exact` is `false` and `value` is an estimate: the sum of
exact counts from already-populated segments plus an extrapolation of average line density
(lines-per-byte from scanned segments) over the remaining unscanned byte range.  As more
segments are populated the estimate converges; once every segment is populated, `exact` becomes
`true` and `value` is authoritative.  Callers such as scroll bars or "go to line" dialogs that
need *some* number immediately — and can tolerate a refinement later — use this API naturally.

**Background scan and eventing.** weaver does not own threads.  To improve the estimate
proactively (e.g. for a scroll bar that wants a reasonable total line count before the user
has navigated the whole file), the caller calls a method such as `scan_next_segment()` from a
background thread.  Each call populates one segment and, if the line count estimate changed, fires
a change notification.

Notifications are delivered via a caller-supplied channel (`mpsc::Sender` or equivalent) rather
than a registered callback, so weaver is decoupled from any particular threading or async model.
The caller drains the channel on the UI thread and responds to each notification.

The notification surface covers at minimum:

- **Line count changed** — a new estimate is available, or `exact` became `true`.
- **Region became exact** — a specific segment range is now fully populated (useful for
  invalidating a line-number gutter over that region).
- **Map invalidated by edit** — specific segments were marked dirty after a branch edit (the
  caller may want to re-query those lines immediately or defer until next access).

The channel is optional: callers that do not supply one receive no notifications and drive
population entirely on-demand through normal queries.

### Encoding-Only Mode — Character Boundary Sync Points

In encoding-only mode the internal sync-point index serves the role that line boundaries serve
in editor mode: it gives callers a way to begin decoding at a known-good position without
scanning from the start of the file.

**Why sync points are necessary.** For UTF-8 and UTF-16 any byte offset can be tested for
boundary status in O(1) — UTF-8 by inspecting the leading-byte pattern (`0xxxxxxx`,
`110xxxxx`, `101xxxxx`…), UTF-16 by requiring even alignment and checking for surrogate pairs.
No pre-computed index is needed for these encodings.  For DBCS encodings (Shift-JIS, GBK,
EUC-JP, EUC-KR, Big5) this is not possible: a byte's role as a lead or trail byte cannot be
determined without context from the preceding byte, so an arbitrary offset may be mid-character.

**Why line terminators are sufficient sync points for DBCS encodings.** In every DBCS encoding
supported by `encoding_rs`, `0x0A` (LF) and `0x0D` (CR) are never valid trail bytes — they
fall below the minimum trail-byte value for all of these encodings.  A line terminator therefore
always marks a character boundary.  The sync-point index records these naturally; extra
boundaries at a configurable density (e.g. every 256 bytes) can be recorded as well for files
with very long lines.

**The API surface for encoding-only mode.**

```rust
// Find the nearest confirmed character boundary at or before `offset`.
fn nearest_sync_point(offset: BranchOffset) -> SyncPoint;

// Iterate decoded characters forward from a sync point.
// Yields (char_start: BranchOffset, ch: char, char_end: BranchOffset).
fn chars_from(sync: SyncPoint) -> impl Iterator<Item = (BranchOffset, char, BranchOffset)>;

// O(1) boundary test where the encoding allows it (UTF-8, UTF-16).
// For DBCS encodings always returns false unless `offset` is a recorded sync point.
fn is_boundary(offset: BranchOffset) -> bool;

// Re-encode a Unicode string to source-encoding bytes for insertion.
fn encode(text: &str) -> Result<Vec<u8>>;
```

**How a rowan-compatible crate uses this.** Green tree leaf nodes store `BranchOffset` ranges
(the same coordinate space as the `redwing` branch).  When a parser or syntax highlighter needs
the decoded text of a node it calls `chars_from(nearest_sync_point(node.start))` and consumes
up to `node.end`.  When it inserts or replaces text it calls `encode()` to obtain the branch
bytes to splice.  The rowan-like crate never sees raw encoding bytes and never needs to know
which encoding is in use; weaver is the complete encoding boundary.

**Sync-point population follows the same lazy pattern as the line map.** The index is built
on demand as offsets are queried, with `scan_next_segment()` available for background
pre-population.  The same eventing channel used for line-count notifications carries sync-point
population events in encoding-only mode.

### Character Encoding

`weaver` uses [`encoding_rs`](https://crates.io/crates/encoding_rs) for decoding and re-encoding.
Decoded `String` values produced by `weaver` are always Rust-native UTF-8; the encoding is an
attribute of the *storage representation*, not of the in-memory API.  Re-encoding uses the same
declared encoding: when a caller writes new text into a node, `weaver` re-encodes it in the source
encoding before producing the byte splice handed to `redwing`.

**Encode/decode symmetry.** `encoding_rs` implements the WHATWG Encoding Standard, which defines
*decoding* for all ~40 encodings it covers but *encoding* (bytes out) for only a subset.
Several legacy encodings are **decode-only by design**: the spec maps them to the `replacement`
encoding, whose decoder always emits `U+FFFD` and which has no usable encoder.  The affected
encodings include ISO-2022-CN, HZ-GB-2312, ISO-2022-KR, BOCU-1, SCSU, and UTF-7 (the last of
which is not in the spec at all).  For practical purposes this is rarely a problem — every
encoding that `chardetng` could plausibly return for real-world files (UTF-8, UTF-16LE/BE,
windows-125x, ISO-8859-x, Shift_JIS, EUC-JP, GBK, EUC-KR, Big5, GB18030) has both a working
decoder and encoder in `encoding_rs`.  However, `weaver` must validate at open time that the
detected encoding has an encoder available before entering a mode that will produce write-back
output; if it does not, read-only / sed-scan mode (which never re-encodes) remains valid.

#### Encoding Detection

Rather than requiring the caller to know the source encoding in advance, `weaver` detects it
automatically at open time.  The detection pipeline is:

1. **Probe read.** `weaver` reads up to a configurable number of bytes from the start of the
   source (default: 8 192 bytes) into a small local buffer.  This is the *only* speculative
   buffering `weaver` ever does; the probe buffer is discarded once the encoding is chosen.

2. **BOM check.** If BOM detection is enabled (default: on), the probe buffer is passed to
   `encoding_rs::Encoding::for_bom()`.  `encoding_rs` recognises the UTF-8 BOM (`EF BB BF`),
   the UTF-16LE BOM (`FF FE`), and the UTF-16BE BOM (`FE FF`).  A detected BOM takes priority
   over all other detection heuristics.  `weaver` records the BOM's byte length so it can be
   skipped or forwarded as the caller requests (see *BOM pass-through* below).

3. **Heuristic detection.** If no BOM is found (or BOM detection is disabled), the probe buffer
   is passed to an `EncodingDetector` implementation.  The built-in default wraps
   [`chardetng`](https://crates.io/crates/chardetng) and calls
   `EncodingDetector::guess(tld: None, allow_utf8: true)`.  The `allow_utf8: true` flag
   instructs `chardetng` to return UTF-8 for content that is consistent with UTF-8, rather than
   falling back to a legacy single-byte encoding.

4. **Result.** Either path yields a `&'static encoding_rs::Encoding`, which becomes the encoding
   used for all subsequent decode and re-encode operations on this source.

#### The `EncodingDetector` Trait

`chardetng` is the default but is not mandatory.  `weaver` defines an `EncodingDetector` trait
so that callers can supply their own detector:

```rust
/// Abstraction over byte-sequence–based character encoding detectors.
///
/// The default implementation wraps `chardetng::EncodingDetector`.
pub trait EncodingDetector {
    /// Feed `data` into the detector.  Set `last` to `true` on the final call.
    fn feed(&mut self, data: &[u8], last: bool);

    /// Return the detected encoding.  Must not be called before at least one `feed`.
    fn guess(&self) -> &'static encoding_rs::Encoding;
}
```

The trait is object-safe; callers pass a `Box<dyn EncodingDetector>` in the initialisation
options when they want to override the default.

#### Initialisation Options

All detection behaviour is controlled by a configuration struct supplied at open time.  Every
field has a sensible default so that the common case — open a text file, detect encoding
automatically — requires no configuration:

| Option | Type | Default | Effect |
|---|---|---|---|
| `probe_len` | `usize` | `8_192` | Bytes read for encoding detection and line-ending detection |
| `bom_detection` | `bool` | `true` | Honour a leading BOM if present |
| `bom_pass_through` | `bool` | `false` | Include the BOM in downstream output |
| `allow_utf8` | `bool` | `true` | Hint passed to `chardetng::guess`; prefer UTF-8 for compatible content |
| `forced_encoding` | `Option<&'static Encoding>` | `None` | Skip detection entirely; use this encoding |
| `detector` | `Option<Box<dyn EncodingDetector>>` | `None` | Override the default `chardetng` detector |
| `decode_error_policy` | `DecodeErrorPolicy` | `Fatal` | Behaviour on invalid byte sequences (see *Decode Error Policy*) |
| `insert_line_ending` | `Option<LineEnding>` | `None` (auto-detect) | Byte sequence used when inserting a logical line break; `None` detects from probe buffer, fallback `LF` |

When `forced_encoding` is `Some`, the probe read, BOM check, and heuristic detection steps are
all skipped.

#### BOM Pass-Through

When a BOM is detected, `weaver` records its byte span.  Two behaviours are then possible:

- **Strip (default, `bom_pass_through: false`).** The BOM bytes are excluded from the logical
  text view.  In sed mode they are not forwarded to the output sink.  In editor mode the line
  map and tree start at the first byte after the BOM.
- **Forward (`bom_pass_through: true`).** The BOM bytes are treated as part of the content and
  passed through unchanged.  This is useful when the goal is a byte-for-byte copy with only
  targeted replacements inside the text.

#### Known Limitation: UTF-32 Is Not Supported

Neither `encoding_rs` nor `chardetng` supports UTF-32.  Both crates are implementations of the
[WHATWG Encoding Standard](https://encoding.spec.whatwg.org/), which was designed for the web
where UTF-32 has essentially no presence and was deliberately excluded from the specification.

There are two compounding reasons this is hard to add later:

- **BOM collision.** The UTF-32LE BOM (`FF FE 00 00`) begins with the same two bytes as the
  UTF-16LE BOM (`FF FE`).  `encoding_rs::Encoding::for_bom()` checks only for the three WHATWG
  BOMs; it will misidentify a UTF-32LE file as UTF-16LE, interpreting the trailing null bytes as
  content rather than as part of the BOM.  Correct UTF-32 BOM detection must be done *before*
  calling `for_bom()`, checking for the full four-byte sequence first.
- **No decoder.** Because `encoding_rs` has no UTF-32 `Encoding` variant, there is no
  `encoding_rs`-compatible decode/re-encode path available.  A caller requiring UTF-32 support
  must supply both a custom `EncodingDetector` implementation *and* a separate decode pipeline
  outside of `encoding_rs`.

For now, UTF-32 content will be mishandled silently.  If UTF-32 support becomes a requirement,
the `EncodingDetector` trait and the initialisation options are the intended extension points.

#### Decode Error Policy and Misdetection Recovery

`chardetng` does not expose a confidence score.  On ambiguous input it makes a best guess and
commits to it; there is no "low confidence" signal that `weaver` can act on.  If that guess is
wrong, the error will manifest as invalid byte sequences during decoding, potentially far into the
file.  The appropriate response depends on the operating mode and is controlled by a
`DecodeErrorPolicy` value in the initialisation options:

| Policy | Behaviour |
|---|---|
| `Fatal` *(default)* | Return an error at the first invalid byte sequence |
| `Substitute` | Replace invalid sequences with `U+FFFD`, continue |
| `ValidateFirst` | Editor mode only — validate the full source before starting; abort early if invalid |
| `ContinuousDetection` | Keep detector alive during scan; abort (sed) or restart from byte 0 (editor) if opinion changes |

**Sed mode constraints.** In practice, sed mode output goes to one of two places: an in-memory
delta/byte buffer that is accumulated and then committed, or a temporary file that is written
incrementally and later renamed over the original.  In neither case is an abort a data-loss
event — the original source, held in the `redwing` branch, is never mutated.  An abort costs
*time* (the scan work done so far) but not data.  The caller simply discards the partial output
buffer or deletes the temporary file and retries.  This is a materially better situation than
writing to a destructive sink like an in-place overwrite or a pipe, and it means the abort
strategies below are all recoverable.

**Replacement size and buffering requirements.** The need for any output buffer at all is
driven by whether replacement byte sequences are larger than the regions they replace:

- **Same size or smaller.** If every replacement produces no more bytes than it consumes,
  the output could theoretically be written back into the source file without a secondary
  buffer — the write head never overtakes the read head.  **This must never be done.**
  If the process is killed, if the system crashes, or if power is lost at any point during a
  partial in-place write, the source file is left in an irrecoverably garbled state with no
  way to reconstruct the original.  This is the same class of failure that drove "binary
  document" formats like the original `.doc` format to implement full transactional
  database-within-a-file storage — the problem is not esoteric, it is fundamental.  `weaver`
  does not expose an in-place write mode.  The correct pattern is always: write to a temporary
  file, then rename atomically over the original.
- **Larger replacements.** As soon as any replacement grows the byte sequence, a temporary
  file or in-memory buffer is structurally required regardless — the output cannot physically
  fit behind the read head.  This is the common case and the one the output pipeline is
  designed around.

The safe output pattern for all sed-mode operations is therefore: write incrementally to a
temporary file in the same directory as the target (same directory ensures the rename is on the
same filesystem and therefore atomic on all POSIX and NTFS systems), then rename over the
original only when the full output is complete and validated.  An abort at any point before the
rename leaves the original untouched.

The abort strategies below apply to the buffered-output case:

1. **Abort (`Fatal` policy, default).** Surface an error; the caller discards the partial output,
   adjusts the initialisation options (e.g. sets `forced_encoding`), and restarts.  No data is lost.
2. **Substitute and continue (`Substitute` policy).** Emit `U+FFFD` in place of invalid
   sequences and keep streaming.  Output is complete but may be semantically corrupt.  Useful
   for diagnostic or display purposes; not appropriate for lossless round-trip or search/replace
   correctness.  Must be explicitly opted into.
3. **Continuous detection + early abort.** Keep the `EncodingDetector` alive and continue
   feeding it bytes during the scan.  If the opinion changes, abort immediately — before
   `encoding_rs` necessarily hits a hard invalid-byte error.  The abort point is earlier and the
   error is more informative (opinion changed at byte N, likely encoding is X).  Combined with
   the `Fatal` policy and the "output to temp file" pattern, this gives clean, safe failure with
   a precise diagnosis.

The fundamental triangle in sed mode is: *auto-detection*, *streaming output*, and
*guaranteed correctness on misdetection* — only two of the three are achievable simultaneously.
In practice this is manageable because the output target is almost always a recoverable buffer
or temporary file, making abort + retry the natural resolution.

**Editor mode options.** Because editor mode never commits output until the caller explicitly
requests it, recovery is more tractable.  However, the read-ahead pattern for line-map
construction means that by the time a misdetection is discovered, a substantial segment of the
line map may already have been built using the wrong encoding.  On an opinion change or decode
error, that state must be discarded and rebuilt from byte 0 — the cost is proportional to how
far the read-ahead had progressed:

1. **Abort (`Fatal` policy, default).** Surface an error; the caller retries with `forced_encoding`.
2. **Substitute (`Substitute` policy).** As above.
3. **Validate-first (`ValidateFirst` policy).** Before building the line map or tree, make a
   forward-only validation pass through the full branch via `read_at`.  If any invalid sequences
   are found, abort before returning control to the caller.  This costs an extra linear scan but
   eliminates the possibility of discovering the error midway through an interactive editing
   session.  Because `redwing`'s `read_at` is demand-driven and does not require the file to be
   fully resident in memory, the scan's memory cost is bounded to the sliding decode window.
4. **Larger probe window.** Setting `probe_len` to 64 KiB or more reduces misdetection
   probability for files with ambiguous byte distributions near the start.  This is a
   probabilistic mitigation, not a guarantee.
5. **Continuous detection (`ContinuousDetection` policy, editor mode only).** Keep the
   `EncodingDetector` alive after the initial probe and continue feeding it bytes as the line-map
   read-ahead progresses.  After each fed chunk, poll `guess()`.  If the opinion changes, discard
   all in-memory state (line map segments, tree nodes, decoded strings) and restart from byte 0
   with the new encoding.  The cost is wasted work proportional to how far the read-ahead had
   advanced when the opinion changed; the result is correct.  Because the line-map read-ahead can
   run considerably ahead of the visible cursor, early opinion changes are caught before they
   propagate into user-visible coordinates.  The `EncodingDetector` trait already supports this
   pattern without API changes.

**A caveat on chardetng's convergence.** `chardetng::EncodingDetector::feed()` returns `true`
when the detector believes it has seen enough data and further feeding is unnecessary.  For most
encodings — and especially for UTF-8 with `allow_utf8: true` — this happens within the first few
kilobytes.  After that point, continued feeding typically does not change the result of `guess()`.
The continuous-detection policy therefore provides the most benefit for ambiguous single-byte
encoding pairs (e.g. windows-1252 vs. windows-1251 vs. ISO-8859-1) where more data genuinely
shifts the probability distribution, and the least benefit for multi-byte encodings where
chardetng has already committed.  A custom `EncodingDetector` backed by a detector that exposes
continuous confidence scores (e.g. ICU's charset detection) could make the restart decision more
precisely.

### Green Tree

The green tree is a tree of nodes and tokens describing the high-level structure of the text.
Nodes are internal and own a list of children; tokens are leaves that directly cover a contiguous
byte span in the source.  The green tree is immutable and reference-counted so that multiple red
trees can share the same structural description.

The granularity of the green tree is left to the consumer: `weaver` defines the mechanism but not
any particular grammar.  A consumer might choose a tree as coarse as `[BOM] [line]* [EOF]` or as
fine-grained as a full language grammar.

### Red Tree

The red tree is a navigable, positioned view over a green tree.  Each red node knows its absolute
byte offset in the source and carries a reference to the underlying `redwing` `DerivedBranch`.
Edit operations on the red tree translate decoded-text edits into byte-level deltas recorded on the
branch.

Because the branch is a `redwing` `DerivedBranch`, all edits are non-destructive: they can be
inspected, merged, conflicted-upon, and discarded without touching the original bytes.  Committing
the branch to storage is an explicit, final step owned by the caller.

## Memory Model

`weaver` operates under an explicit memory discipline:

- **No single internal allocation may exceed a compile-time constant.**  The target ceiling is
  256 MiB.  This is not a hard limit enforced at runtime (Rust does not provide that guarantee)
  but a design constraint: no data structure or algorithm inside `weaver` is permitted to
  *require* an allocation larger than this in its normal operating path.  Structures that would
  need to grow beyond the ceiling must be segmented.
- **Sed-mode pipeline depth is bounded.**  The internal buffer between the source scanner and the
  output `Write` sink is capped at a small constant; 128–256 KiB is the target.  This ensures
  that streaming a terabyte file never accumulates more than a few hundred kilobytes of
  in-flight data.
- **Line-map segments are fixed-size.**  Each segment of the multi-level line map covers a
  bounded number of lines and a bounded number of bytes, so individual segment allocations are
  predictably small regardless of file size.
- **Caller-requested materialisation is the caller's responsibility.**  If a caller asks
  `weaver` for a `Vec<u8>` of the full processed output, `weaver` will attempt to produce it.
  The decision to materialise an arbitrarily large buffer belongs to the caller, not to
  `weaver`; `weaver` will not refuse the request on memory grounds, but it will not buffer
  speculatively on the caller's behalf either.
- **Storage offloading for large modified content is an open question.**  For very large files
  with many accumulated deltas, it may eventually be necessary to spill the `redwing` delta log
  (or the materialised output) to temporary storage.  Whether that mechanism lives in `weaver`,
  in `redwing`, or as an optional coordination layer between the two is left open for the
  detailed design phase.

## Design Goals

- **Encoding-agnostic.** First-class support for UTF-8, UTF-16LE, UTF-16BE, and any other encoding
  surfaced by `encoding_rs`.  No silent transcode, no lossy normalisation.
- **Lossless round-trip.** Reading a file and writing it back with no edits produces a bit-identical
  result.  Line endings, BOMs, and any other byte-level details are preserved.
- **Non-destructive editing.** All edits are deltas held in a `redwing` branch.  The original bytes
  are never mutated until the caller explicitly commits.
- **Incremental.** The line map and green tree support incremental update after small edits; a
  single-line change does not require a full re-scan.
- **Byte-offset primary.** All internal arithmetic is in bytes.  Decoded-character and line/column
  coordinates are derived views, not the primary representation.
- **Bounded internal allocations.** No internal data structure requires a single allocation
  exceeding 256 MiB.  Anything that would need to grow beyond that is segmented.
- **Two modes, one crate.** Sed mode and editor mode share the encoding layer and the `redwing`
  integration, but neither mode pays the overhead of the other's data structures.

## Status

Early design phase.  No public API is stable.  See `CHECKLIST.md` in this directory for the
current work plan.
