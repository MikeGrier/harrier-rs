// Copyright (c) 2026, Michael Grier

//! `rr2` — in-place regex substitution using harrier's normalised-view infrastructure.
//!
//! Usage (file mode):
//!     rr2 <file> <pattern> <replacement>
//!
//! Usage (generate mode):
//!     rr2 --gen <spec> <file> <pattern> <replacement>
//!
//! `<pattern>` is a [`regex::bytes`] regular expression applied to the
//! **normalised** (LF-only) view of the file.  `<replacement>` is a
//! replacement template that may reference capture groups via `$0`, `$1`,
//! `$name`, etc.  Use `$$` to produce a literal `$`.
//!
//! # File mode
//!
//! The program:
//!   1. Opens `<file>` and memory-maps it.
//!   2. Wraps the map in a redwing thicket; `b1` is the initial read-only branch.
//!   3. Opens the branch with [`harrier::source::Source`] to detect encoding,
//!      BOM, and the dominant line-ending convention.
//!   4. Materialises the entire file as a normalised (LF-only) [`View`] via
//!      [`harrier::lines::Lines::view_range`].
//!   5. Runs the regex over the normalised bytes and collects every match as
//!      a source-coordinate splice descriptor.  Each replacement template is
//!      expanded (capture groups resolved) in normalised space and then
//!      denormalised with [`DenormaliseWriter`] so that `\n` bytes are
//!      restored to the file's dominant terminator.
//!   6. Forks `b1` → `b2` and applies the collected splices in **reverse
//!      source order** — because each splice only shifts bytes after its
//!      position, earlier-offset splices can safely reuse the original b1
//!      source coordinates.
//!   7. Writes the result to a `NamedTempFile` in the same directory (same
//!      filesystem, guaranteeing an atomic rename).
//!   8. Renames `<file>` to `<file>.bak`, then `persist()`s the temp file
//!      over `<file>`.
//!   9. Prints a one-line summary: original path, backup path, replacement
//!      count.
//!
//! # Generate mode (`--gen <spec>`)
//!
//! Instead of reading `<file>`, content is generated synthetically according to
//! `<spec>`, a comma-separated list of `key=value` pairs:
//!
//! | Key | Values | Default |
//! |-----|--------|---------|
//! | `length` | integer bytes | 1024 |
//! | `line_ending` | `lf`, `crlf`, `cr` | `lf` |
//! | `ambiguity` | integer bytes of ASCII before encoding marker | 0 |
//! | `marker` | `none`, `latin1_copyright` (0xA9), `win1252_euro` (0x80), `utf8_bom`, `utf16le_bom`, `utf16be_bom` | `none` |
//! | `error_at` | integer byte offset for injection (omit = no error) | — |
//! | `error` | `invalid_seq` (0xFF 0xFF), `truncated_seq` (0xC2), `overlong` (0xC0 0x80), `surrogate` (0xED 0xA0 0x80) | `invalid_seq` |
//!
//! Generated lines alternate between `"TARGET000000"` (every 10th line) and
//! `"fillerNNNNNN"` filler, with the configured line ending.  This embeds a
//! known search target at predictable positions.
//!
//! The result is written to `<file>` (created or overwritten) without the
//! `.bak` rename, since no original source exists to preserve.
use std::{env, fs, io::Write as _, path::PathBuf, sync::Arc};

use harrier::{
    denormalise::DenormaliseWriter,
    encoding::{LineEnding, SourceConfig},
    source::Source,
};
use memmap2::MmapOptions;
use regex::bytes::Regex;
use tempfile::NamedTempFile;

// ── GenSpec: synthetic content descriptor ─────────────────────────────────────

/// Parameters for synthetic content generation (`--gen <spec>`).
pub struct GenSpec {
    /// Total generated content length in bytes (content may be a few bytes
    /// longer due to line boundaries; never truncated to avoid splitting CRLF).
    pub length: usize,
    /// Raw bytes appended after each line body.
    pub line_ending: &'static [u8],
    /// Bytes of pure ASCII content before `encoding_marker` is injected.
    /// `usize::MAX` suppresses the marker even if one is configured.
    pub ambiguity: usize,
    /// Raw bytes injected at the `ambiguity`-byte boundary to distinguish the
    /// encoding (e.g. 0xA9 for Latin-1 copyright ©).
    /// An empty slice means no marker is injected.
    pub marker: &'static [u8],
    /// Byte offset at which `error_bytes` are injected.
    /// `None` means no error injection.
    pub error_at: Option<usize>,
    /// Raw bytes injected at `error_at` to represent an encoding error.
    /// An empty slice means no error is injected.
    pub error_bytes: &'static [u8],
}

/// Parse a `--gen` spec string into a [`GenSpec`].
///
/// `s` is a comma-separated list of `key=value` pairs.  Unknown keys are
/// rejected so typos are caught early.
fn parse_gen_spec(s: &str) -> Result<GenSpec, Box<dyn std::error::Error>> {
    let mut spec = GenSpec {
        length: 1024,
        line_ending: b"\n",
        ambiguity: usize::MAX, // default: no marker injection
        marker: b"",
        error_at: None,
        error_bytes: b"\xff\xff", // default error kind if error_at is given
    };
    for pair in s.split(',') {
        let (k, v) = pair
            .split_once('=')
            .ok_or_else(|| format!("malformed key=value pair: {pair:?}"))?;
        match k.trim() {
            "length" => spec.length = v.trim().parse()?,
            "line_ending" => {
                spec.line_ending = match v.trim().to_ascii_lowercase().as_str() {
                    "lf" => b"\n",
                    "crlf" => b"\r\n",
                    "cr" => b"\r",
                    _ => return Err(format!("unknown line_ending value: {v:?}").into()),
                }
            }
            "ambiguity" => spec.ambiguity = v.trim().parse()?,
            "marker" => {
                spec.marker = match v.trim().to_ascii_lowercase().as_str() {
                    "none" => b"",
                    "latin1_copyright" => b"\xa9",
                    "win1252_euro" => b"\x80",
                    "utf8_bom" => b"\xef\xbb\xbf",
                    "utf16le_bom" => b"\xff\xfe",
                    "utf16be_bom" => b"\xfe\xff",
                    _ => return Err(format!("unknown marker value: {v:?}").into()),
                }
            }
            "error_at" => spec.error_at = Some(v.trim().parse()?),
            "error" => {
                spec.error_bytes = match v.trim().to_ascii_lowercase().as_str() {
                    "invalid_seq" => b"\xff\xff",
                    "truncated_seq" => b"\xc2", // UTF-8 2-byte lead, no continuation
                    "overlong" => b"\xc0\x80",  // overlong NUL — invalid in UTF-8
                    "surrogate" => b"\xed\xa0\x80", // U+D800 in UTF-8 — invalid
                    _ => return Err(format!("unknown error value: {v:?}").into()),
                }
            }
            _ => return Err(format!("unknown gen-spec key: {k:?}").into()),
        }
    }
    Ok(spec)
}

/// Generate synthetic file content from a [`GenSpec`].
///
/// Lines alternate between `"TARGET000000"` (every 10th line, starting at 0)
/// and `"fillerNNNNNN"` filler lines, each terminated by `spec.line_ending`.
/// This embeds a known search target at predictable positions.
///
/// The encoding marker is injected after `spec.ambiguity` bytes of ASCII, and
/// the error bytes are injected at `spec.error_at` if set.  Injections are
/// appended as raw bytes between normal lines; they do not split a line.
pub fn generate_content(spec: &GenSpec) -> Vec<u8> {
    if spec.length == 0 {
        return Vec::new();
    }
    let mut out: Vec<u8> = Vec::with_capacity(spec.length + 128);
    let mut line_num: usize = 0;
    let mut marker_done = spec.marker.is_empty() || spec.ambiguity == usize::MAX;
    let mut error_done = spec.error_bytes.is_empty() || spec.error_at.is_none();

    while out.len() < spec.length {
        let pos = out.len();

        if !marker_done && pos >= spec.ambiguity {
            out.extend_from_slice(spec.marker);
            marker_done = true;
            continue;
        }

        if !error_done
            && let Some(off) = spec.error_at
            && pos >= off
        {
            out.extend_from_slice(spec.error_bytes);
            error_done = true;
            continue;
        }

        let body: &[u8] = if line_num.is_multiple_of(10) {
            b"TARGET"
        } else {
            b"filler"
        };
        out.extend_from_slice(body);
        let num = format!("{line_num:06}");
        out.extend_from_slice(num.as_bytes());
        out.extend_from_slice(spec.line_ending);
        line_num += 1;
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ── argument parsing ──────────────────────────────────────────────────────
    //
    // Two modes:
    //   file mode:     rr2 <file> <pattern> <replacement>          (4 args)
    //   generate mode: rr2 --gen <spec> <file> <pattern> <repl>   (6 args)

    let args: Vec<String> = env::args().collect();

    let (gen_spec, file_path, pattern, replacement): (Option<GenSpec>, PathBuf, &str, &[u8]) =
        if args.get(1).map(String::as_str) == Some("--gen") {
            if args.len() != 6 {
                eprintln!(
                    "Usage: {} --gen <spec> <file> <pattern> <replacement>",
                    args[0]
                );
                std::process::exit(1);
            }
            (
                Some(parse_gen_spec(&args[2])?),
                PathBuf::from(&args[3]),
                &args[4],
                args[5].as_bytes(),
            )
        } else {
            if args.len() != 4 {
                eprintln!("Usage: {} <file> <pattern> <replacement>", args[0]);
                std::process::exit(1);
            }
            (None, PathBuf::from(&args[1]), &args[2], args[3].as_bytes())
        };

    // ── acquire input branch ──────────────────────────────────────────────────
    //
    // In generate mode: build the thicket from synthesised bytes (no disk I/O
    //   required; NamedTempFile / .bak dance is skipped).
    // In file mode:     memory-map the file and build the thicket from the map.

    enum Input {
        Generated,
        Mapped,
    }

    let (b1, input_kind) = if let Some(ref spec) = gen_spec {
        let content = generate_content(spec);
        (
            redwing::make_thicket_from_bytes(content).main(),
            Input::Generated,
        )
    } else {
        let file = fs::File::open(&file_path)?;
        // SAFETY: the mapped bytes are only ever read through the Branch read
        // API.  All mutations are applied through a forked branch that stores
        // deltas separately; the underlying mapping is never written.
        let mmap = unsafe { MmapOptions::new().map(&file) }?;
        drop(file);
        (redwing::make_thicket_from_mmap(mmap).main(), Input::Mapped)
    };

    let file_len = b1.byte_len();

    // ── open with harrier to detect encoding and line-ending ───────────────────

    let source = Source::new(Arc::clone(&b1), SourceConfig::default())?;
    let line_ending = source.line_ending();

    // ── materialise a normalised (LF-only) view of the entire file ───────────

    let lines = source.as_lines()?;
    let view = lines.view_range(0..file_len)?;

    // ── compile the pattern and collect splice descriptors ────────────────────

    let re = Regex::new(pattern)?;

    // Each match is translated from normalised coordinates to source coordinates
    // and the replacement template is expanded then denormalised so that `\n`
    // bytes use the file's dominant line terminator.
    struct Splice {
        source_start: u64,
        source_len: u64,
        content: Vec<u8>,
    }

    let mut splices: Vec<Splice> = re
        .captures_iter(&view.bytes)
        .map(|caps| {
            let m = caps.get(0).unwrap();
            let norm_start = m.start() as u64;
            let norm_end = m.end() as u64;

            // Translate normalised offsets → source (b1-absolute) offsets.
            let source_start = view.byte_range_start() + view.offset_map.to_source(norm_start);
            let source_end = view.byte_range_start() + view.offset_map.to_source(norm_end);
            let source_len = source_end - source_start;

            // Expand capture-group back-references in the LF-normalised template.
            let mut norm_repl: Vec<u8> = Vec::new();
            caps.expand(replacement, &mut norm_repl);

            // Denormalise: substitute each \n with the file's dominant terminator.
            let content = denormalise_bytes(&norm_repl, line_ending);

            Splice {
                source_start,
                source_len,
                content,
            }
        })
        .collect();

    let replacement_count = splices.len();

    // ── fork b1 and apply splices in reverse source order ────────────────────

    // Applying in reverse order means each splice only shifts bytes *after*
    // its position, so earlier-offset splices can safely reuse b1 coordinates.
    let b2 = b1.fork();
    splices.sort_unstable_by_key(|b| std::cmp::Reverse(b.source_start));
    for s in &splices {
        b2.splice(s.source_start, s.source_len, &s.content)?;
    }

    let out_bytes = redwing::materialize(&*b2)?;

    // Release all branch and view references before any file-system work.
    // On Windows a memory-mapped file cannot be renamed while a view remains open.
    drop(splices);
    drop(view);
    drop(lines);
    drop(b2);
    drop(b1);

    // ── write output ──────────────────────────────────────────────────────────

    match input_kind {
        Input::Generated => {
            // Generate mode: write the result directly to <file>.  No original
            // file exists, so the .bak rename is skipped.
            fs::write(&file_path, &out_bytes)?;
            println!(
                "(generated) -> {} ({} replacement{})",
                file_path.display(),
                replacement_count,
                if replacement_count == 1 { "" } else { "s" },
            );
        }
        Input::Mapped => {
            // File mode: write temp, rename original to .bak, persist temp.
            let dir = file_path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(std::path::Path::new("."));

            // NamedTempFile::new_in creates a randomly-named file in `dir`
            // (same filesystem as `file_path`) and deletes it automatically
            // if we return early without persisting.
            let mut tmp = NamedTempFile::new_in(dir)?;
            tmp.write_all(&out_bytes)?;

            let bak_path = PathBuf::from(format!("{}.bak", file_path.display()));
            fs::rename(&file_path, &bak_path)?;

            // persist() atomically renames the temp file to `file_path`. If
            // it fails, restore the backup so original content is not lost.
            if let Err(e) = tmp.persist(&file_path) {
                let _ = fs::rename(&bak_path, &file_path);
                return Err(e.error.into());
            }

            println!(
                "{} -> {} ({} replacement{})",
                file_path.display(),
                bak_path.display(),
                replacement_count,
                if replacement_count == 1 { "" } else { "s" },
            );
        }
    }

    Ok(())
}

/// Expand a normalised (LF-only) byte slice to use the file's dominant
/// line terminator.
///
/// Each `\n` byte is passed through [`DenormaliseWriter`] backed by an
/// infinite repeat of `le`, so every newline in the replacement becomes the
/// file-native ending.  Non-newline bytes are forwarded verbatim.
fn denormalise_bytes(norm: &[u8], le: LineEnding) -> Vec<u8> {
    let mut dw = DenormaliseWriter::new(Vec::with_capacity(norm.len()), std::iter::repeat(le));
    // Vec<u8> never returns I/O errors; unwrap is safe.
    dw.write_all(norm).unwrap();
    // into_inner rather than finish: the repeat iterator has no "surplus"
    // terminators to flush.
    dw.into_inner()
}
