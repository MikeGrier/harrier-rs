// Copyright (c) 2026, Michael Grier

pub mod buffer;
pub mod chars;
pub mod denormalise;
pub mod encoded;
pub mod encoding;
pub mod line_count;
pub mod line_map;
pub mod line_map_event;
pub mod line_map_segment;
pub mod lines;
pub mod mallard_bridge;
pub mod offset_map;
pub mod source;
pub mod view;

#[cfg(test)]
mod tests;
