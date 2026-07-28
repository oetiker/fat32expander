//! Resize orchestration: size calculation, cluster relocation, crash recovery.
//!
//! [F-R44] This module and all its leaves are private; the crate root decides
//! what of it is public. Callers drive a resize through
//! `fat32expander::resize_fat32` and query one through
//! `fat32expander::get_fs_info`.

mod calculator;
mod checkpoint;
mod executor;
mod fault;
mod fsinfo;
mod options;
mod relocator;

#[cfg(test)]
mod testutil;

// The resize entry point and its data types. `SizeCalculation` is reachable
// because `ResizeResult` hands one out.
pub use calculator::SizeCalculation;
pub use executor::resize_fat32;
pub use fsinfo::{get_fs_info, FSInfoReport};
pub use options::{ResizeOptions, ResizeResult};

// The checkpoint record is deliberately not re-exported. `write_checkpoint`,
// `read_checkpoint`, `clear_checkpoint` and `check_for_incomplete_resize` are
// all crate-private, so exporting `ResizeCheckpoint`/`ResizePhase` let an
// external caller build and serialise a crash-recovery record it had no way to
// apply -- the incoherence [F-R44] flags. The types stay `pub` within the crate
// so the executor and the checkpoint tests can name them.
