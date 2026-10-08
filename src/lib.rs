//! In-place FAT32 filesystem expansion.
//!
//! # Public API structure
//!
//! [F-R44] This crate exposes its items on exactly two levels, with a stated
//! difference between them. Before, every module was both `pub` and
//! re-exported, so a type such as `BootSector` was reachable by three paths
//! with nothing saying which one a caller should depend on.
//!
//! * **The crate root is the supported API.** `fat32expander::resize_fat32`,
//!   `::get_fs_info`, `::Device`, `::BootSector` and the rest of the items
//!   re-exported below are what a consumer should name. They change only with
//!   a major version.
//!
//! * **[`fat32`] is the lower-level namespace**, for a caller that wants to
//!   read a boot sector or a FAT table without driving a whole resize. It is
//!   supported but deliberately closer to the on-disk format, so it may gain
//!   and lose items more readily than the root.
//!
//! Everything else -- the `device`, `error`, `resize` and `system` modules, and
//! the `structs`/`operations`/`validation` leaves under [`fat32`] -- is private.
//! An item not re-exported here is not part of the API, which is also what lets
//! the compiler's dead-code analysis see it: a `pub` item re-exported from a
//! library is exempt from that lint, so an unused helper looked reachable and
//! stayed invisible ([F-R34], [F-R35]).
//!
//! The checkpoint record is deliberately *not* exported. `ResizeCheckpoint` and
//! `ResizePhase` were public while every function that reads, writes or applies
//! one is crate-private, so an external caller could build and serialise a
//! crash-recovery record but never act on it.
//!
//! `SizeCalculation` is not exported either ([F-09]). `ResizeResult` used to
//! hand one out, which made every member public, read or not; it now carries
//! the four geometry numbers a caller has a use for as plain fields.
//!
//! [F-26]: `Error`, `ResizeResult` and `ResizeEvent` are `#[non_exhaustive]`,
//! so a new error variant, result field or event is not a breaking change.
//! Match them with a wildcard arm, and read `ResizeResult` by field.

mod device;
mod error;
pub mod fat32;
mod resize;
mod system;

pub use device::Device;
pub use error::{Error, Result};

/// Device write/flush recording, for tests that assert durability ordering.
///
/// Only present under the `device-journal` feature, which no shipped build
/// enables. It is exported because the ordering it exists to check is only
/// visible from an integration test driving [`resize_fat32`] end to end, and
/// integration test crates cannot see the library's `#[cfg(test)]` items.
#[cfg(feature = "device-journal")]
pub use device::journal;

pub use fat32::{BootSector, FSInfo};
pub use resize::{
    get_fs_info, resize_fat32, resize_fat32_with_events, FSInfoReport, ResizeEvent, ResizeOptions,
    ResizeResult,
};
pub use system::{check_not_mounted, check_root, get_block_device_size};
