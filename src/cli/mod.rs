//! The `fat32expander` command apart from its argument parsing and `main`
//! ([F-25]). These modules belong to the binary: they are declared from
//! `src/main.rs`, never from `src/lib.rs`, so the library does not pull in
//! `clap` or `anyhow` ([F-27]).

pub mod exit;
pub mod render;
pub mod resize;
pub mod version;
