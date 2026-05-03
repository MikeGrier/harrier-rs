// Copyright (c) 2026, Michael Grier

//! Unit tests for the `harrier` crate.
//!
//! Each submodule mirrors the source module it tests.  Integration tests
//! (MA-IT-*) live in `crates/harrier/tests/` per Cargo conventions.

mod buffer_tests;
mod chars_tests;
mod denormalise_tests;
mod encoding_tests;
mod line_map_tests;
mod lines_tests;
mod mallard_bridge_tests;
mod offset_map_tests;
mod source_tests;
mod view_tests;
