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
//! `SizeCalculation` runs the same argument in reverse and so *is* exported:
//! it is the type of `ResizeResult::calculation`, a public field, and a public
//! field whose type a caller cannot name is the same incoherence pointing the
//! other way.

mod device;
mod error;
pub mod fat32;
mod resize;
mod system;

pub use device::Device;
pub use error::{Error, Result};
pub use fat32::{BootSector, FSInfo};
pub use resize::{
    get_fs_info, resize_fat32, FSInfoReport, ResizeOptions, ResizeResult, SizeCalculation,
};
pub use system::{check_not_mounted, check_root, get_block_device_size};
