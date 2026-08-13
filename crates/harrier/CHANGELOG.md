# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0](https://github.com/MikeGrier/harrier-rs/compare/harrier-v0.2.0...harrier-v0.3.0) (2026-08-13)


### Features

* bump version to 0.3.0 ([509ed5d](https://github.com/MikeGrier/harrier-rs/commit/509ed5d1392d8db3e5e6c62c3ed5eba74dee767c))
* trigger release-please for harrier 0.3.0 ([b72a31f](https://github.com/MikeGrier/harrier-rs/commit/b72a31f9dec729a32151953221c7e0594b86c88a))

## [0.2.0] - 2026-06-23

### Changed

- **Encoding detection now prefers UTF-8 when the probe is well-formed UTF-8.**
  Before running the `chardetng` heuristic, a probe that is valid UTF-8 and
  free of interleaved NUL bytes is classified as `UTF-8` directly. This removes
  a class of misdetections where UTF-8 text dense with multi-byte sequences
  (box-drawing, em-dashes, arrows, smart quotes) was guessed as
  `windows-1252`. **This changes detection output for existing inputs**: some
  files previously reported as `windows-1252` are now reported as `UTF-8`.
  The behaviour is controlled by the new, default-on
  `SourceConfig::prefer_utf8_when_valid` field.

### Added

- `SourceConfig::prefer_utf8_when_valid` (default `true`) — gate UTF-8 on
  validation instead of the heuristic.
- `SourceConfig::validate_full_stream_utf8` (default `false`) — opt in to
  validating the entire branch (streamed in 128 KiB chunks) before committing
  to the UTF-8 fast path, rather than trusting the probe prefix alone.

### Notes

- `SourceConfig` is now `#[non_exhaustive]`. Downstream crates must construct
  it from `SourceConfig::default()` and assign fields (struct-literal syntax,
  including `..Default::default()`, is no longer permitted cross-crate); future
  field additions will not be breaking.
- Minimum supported Rust version (MSRV) is now declared as `1.95`.
