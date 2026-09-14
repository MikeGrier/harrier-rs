# harrier design notes

Design decisions and rationale for `harrier`. Each entry records a decision,
the reasoning behind it, and any open follow-ups. Module-level docs cite these
where relevant.

---

## Line editing: the line-vs-byte-offset conflict (`line_edit`)

**Status:** landed (mechanism); policy layer intentionally left to consumers.
**Module:** [`crate::line_edit`], reached via [`Source::as_line_editor`].

### The problem

Line-based editing and byte offsets are in fundamental conflict. A line's
identity is defined by its *terminator*, and the `\n` that ends line *N* is the
very same byte that separates it from line *N+1*. So any attempt to encode
"line *N*" as a single byte range forces an ambiguous choice:

- **Include** the terminator in the range → replacing it with unterminated
  content *welds* two logical lines together
  (`A\nB\nC\n`, replace line 2 with `X` → `A\nXC\n`).
- **Exclude** it → you can no longer *delete* a whole line without leaving a
  blank one behind.

[`View::apply`] is deliberately a *verbatim* byte splice, so it cannot resolve
this on the caller's behalf; a naive `replace_lines(range, bytes)` built on raw
offsets reproduces the weld. This surfaced as a real defect in a downstream
consumer (`tpu`), whose line-mode `edit` had to grow terminator-preservation,
end-of-file, and multi-edit-composition logic by hand.

### The decision: expose the terminator *boundary*, split mechanism from policy

harrier owns the **mechanism**; the consumer owns the **policy**.

The mechanism is [`LineEditor`], which for any line range yields a [`LineSpan`]
carrying **both** boundaries plus a flag:

- `content` — the range **excluding** the last line's terminator.
- `full` — the range **including** it (equal to `content` when that line is
  unterminated).
- `terminated` — whether the last line carried a terminator.

With both boundaries in hand, the conflict evaporates and no terminator is ever
guessed:

- *Replace a line's text but keep it a line* → splice `content` (the terminator
  is simply not in the spliced range, so it stays).
- *Delete a line* → splice `full`.
- *Insert a line* → zero-width splice at `content.start` with caller-terminated
  content.

The **policy** — *when* to keep a terminator, how EOF appends behave, how
overlapping intents compose — is not harrier's. It lives in the consumer
(`tpu`). `line_edit_tests` includes a worked `policy_*` composition proving a
strict, weld-free editor is a thin layer over the mechanism (e.g. `replace`
is one line: splice `span.content` with the encoded text — zero terminator
synthesis).

### Key implementation insight

`LineEditor` is built on [`Lines`], harrier's **encoding-aware** line scanner.
An earlier draft was built on [`View`], on the reasoning that `View` already
normalizes to LF, so every terminator is exactly one `\n` in normalized space.
That premise was **wrong for the whole point of the exercise**: the reason to
bring this into harrier is to *share* the terminator detection, and `View`'s
normalization is byte-level (it splits on a bare `0x0A`), so a `View`-based
editor is single-byte-only and silently mis-slices UTF-16 — the exact case
harrier already solves in [`Lines`] via `next_utf16`. Re-detecting terminators
at the byte level would have re-implemented (incorrectly) code that already
exists.

The landed editor therefore consumes the `Lines` iterator once at construction
and records, per line, its **source** byte offsets: `start`, `content_end`
(payload only), `full_end` (payload + the line's real source terminator), and
`terminated`. The arithmetic is encoding-uniform: `Lines` normalizes each
terminator to a single LF *code unit* included in the returned content, so the
payload is `content.len() - unit_size` (`unit_size` = 2 for UTF-16, else 1), and
the source terminator width is `unit_size` for `Lf`/`Cr`, `2 * unit_size` for
`CrLf`, and 0 for the unterminated final line. Offsets accumulate from the BOM
length, so `line_span` returns exact **source** spans for every encoding with no
offset map.

Replacement bytes are shared through the same encoding-aware primitive that
`Lines`/`DenormaliseWriter` use: [`line_ending_bytes`] widens a `LineEnding` to
the encoding's code units (e.g. `Lf` → `[0x0A, 0x00]` under UTF-16LE), and
`DenormaliseWriter` was made code-unit-aware so the substitution side is UTF-16
correct too. `apply` splices caller-provided **source** bytes verbatim onto a
fork of the branch (BOM untouched) — no normalize/denormalize round-trip,
because the caller already works in source coordinates.

The one thing `encoding_rs` cannot do is *encode* text to UTF-16 (it has no
UTF-16 encoder and silently falls back to UTF-8). So
[`encode_line`](crate::line_edit::LineEditor::encode_line) returns
`EncodeUnavailable` for UTF-16 rather than corrupting the file; UTF-16 callers
supply pre-encoded content bytes to `apply` directly. Byte-level UTF-16 edits
(delete, or replace-with-caller-encoded-bytes) work fully.

### API-fit review (recorded from design discussion)

An earlier draft was a monolithic `LineEditor` that materialized the document,
re-parsed lines itself, baked in the terminator/EOF policy, and rebuilt the
whole file per edit. The following critiques and resolutions shaped the landed
design:

1. **"Materializing the whole document violates harrier's laziness."**
   *Resolution:* laziness is an implementation facet, **not** part of the public
   API surface; it exists so common patterns need not instantiate the whole
   line map. For editing, the map must be instantiated at least through the
   edited point anyway. The current `LineEditor` performs one eager [`Lines`]
   scan at construction; reducing that to a lazy, bounded scan is an
   implementation optimization (see *Open follow-ups*), not an API concern.

2. **"Build on harrier's own primitives, not a re-rolled parser."**
   *Resolution:* the landed `LineEditor` is built on [`Lines`], harrier's
   encoding-aware line scanner, and reuses [`line_ending_bytes`] for terminator
   synthesis. (An interim draft used [`View`]; it was replaced precisely because
   it bypassed `Lines`' terminator detection — see *Key implementation insight*.)

3. **"Construction should match the `Source::as_*` family."**
   *Resolution:* the entry point is [`Source::as_line_editor`], alongside
   `as_chars` / `as_lines` / `as_buffer`.

4. **"Why return raw bytes from `render`?"**
   *Resolution:* removed. Edits return a new `Arc<dyn Branch>` from `apply`
   (materialize via `redwing::materialize` when bytes are actually needed),
   matching [`View::apply`]'s persistent-branch model.

5. **"Split what is mechanism (harrier) from what is policy (the consumer)."**
   *Resolution:* this is the central decision above. harrier ships `line_span`
   + `apply` + `encode_line` + `terminator` + `line_ending` (mechanism).
   Terminator/EOF/multi-edit composition stays with the consumer.

6. **"Why exclude UTF-16?"**
   *Resolution:* it is **no longer excluded** for byte-level edits. Building on
   [`Lines`] gives source-coordinate spans with code-unit-aware terminator
   widths, so `line_span`, delete (`full` splice), and replace-with-encoded-bytes
   (`content` splice) all work for UTF-16LE/BE. The residual limit is *text
   insertion*: `encoding_rs` has no UTF-16 encoder, so
   [`encode_line`](crate::line_edit::LineEditor::encode_line) returns
   `EncodeUnavailable` for UTF-16 and callers must supply pre-encoded content
   bytes. Closing that gap needs a UTF-16 encode path (trivial to add — encode
   UTF-8 → UTF-16 code units directly — but deliberately deferred until a
   consumer needs harrier to synthesise UTF-16 text).

### Open follow-ups

- **Lazy, bounded `line_span`.** Construction consumes the whole [`Lines`]
  iterator once, recording per-line source offsets — an eager full scan. For
  very large documents, resolving only the requested line range through
  [`LineMap`] (segmented, on-demand) would avoid scanning the entire file to
  edit a few lines. This requires exposing per-line terminator information from
  `LineMap` (already computed per segment, but not yet a public accessor) so the
  content/`full` boundary can be produced in *source* coordinates without a full
  `Lines` pass.

- **Coordinate spaces — resolved.** The first draft used [`View`]
  (**normalized** coordinates + offset map); the landed editor uses [`Lines`],
  which already yields **source**-coordinate content plus a per-line
  [`LineTerminator`]. `line_span` therefore reports source spans directly, and
  `apply` splices source bytes with no offset-map bridge. The remaining lazy-path
  work above is an optimization, not a coordinate-space mismatch.

- **UTF-16 — byte-level done, text-encode deferred.** Source-coordinate line
  spans with code-unit-aware terminator widths make `line_span`, delete, and
  replace-with-encoded-bytes work for UTF-16LE/BE (covered by
  `line_edit_tests`). The only gap is *synthesising* UTF-16 text:
  [`encode_line`](crate::line_edit::LineEditor::encode_line) returns
  `EncodeUnavailable` for UTF-16 (`encoding_rs` has no UTF-16 encoder), so
  callers currently supply pre-encoded content bytes. A direct UTF-8→UTF-16
  code-unit encode path would close it.

- **Consumer adoption.** `tpu`'s line-mode `edit` currently carries its own
  terminator/EOF/multi-op policy over a private line→byte mapping. Once this API
  is published it can delete that mapping and express its policy as a thin
  `policy_*`-style layer over `LineEditor` (the `line_edit_tests` composition is
  the reference).

[`crate::line_edit`]: crates/harrier/src/line_edit.rs
[`LineEditor`]: crates/harrier/src/line_edit.rs
[`LineSpan`]: crates/harrier/src/line_edit.rs
[`Lines`]: crates/harrier/src/lines.rs
[`LineTerminator`]: crates/harrier/src/lines.rs
[`line_ending_bytes`]: crates/harrier/src/denormalise.rs
[`View`]: crates/harrier/src/view.rs
[`View::apply`]: crates/harrier/src/view.rs
[`LineMap`]: crates/harrier/src/line_map.rs
[`Source::as_line_editor`]: crates/harrier/src/source.rs
