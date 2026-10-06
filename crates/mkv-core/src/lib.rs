//! Core domain and batch pipeline for anime fansub collections.

pub mod assets;
mod cpu;
pub mod domain;
pub mod ebml;
pub mod extract;
pub mod fonts;
pub mod manifest;
pub mod pipeline;
pub mod preset;
pub mod remux;
mod remux_plan;
pub mod request;
pub mod scan;
