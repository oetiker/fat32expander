//! FAT32 on-disk format: structures, parsing, validation and I/O.
//!
//! [F-R44] The three leaves below are private. Everything this module supports
//! is named in the re-export lists, which are therefore the whole surface --
//! `fat32::structs::BootSector` and its siblings are gone, leaving one path per
//! item here. See the crate root for how this namespace differs from it.

mod operations;
mod structs;
mod validation;

// Re-export types from structs
//
// [F-R35] `ClusterId` and `SectorNum` were removed. They were ~117 lines of
// type-safety machinery with zero constructors and zero consumers, and adopting
// them would have meant rewriting them first: `SectorNum::offset` and
// `to_byte_offset` used bare `+`/`*`, which would have put unchecked arithmetic
// back into the paths [F-R7] and [F-R28] had just hardened, and
// `ClusterId::index`'s `saturating_sub(2)` mapped clusters 0 and 1 silently to
// index 0 -- hiding exactly the bug [F-R36] is about rather than catching it.
pub use structs::{fat_entry, BootSector, FSInfo};

// Re-export operations
//
// [F-R34] Seven entry-level and cluster-level helpers were removed from here
// and from `operations.rs`: they were leftovers from the pre-refactor
// cluster-renumbering design, which the current data-shift approach does not
// need, and being `pub` in a library exempted them from the dead-code lint.
// `count_free_clusters` was deliberately kept -- see the note on it.
// [F-15]: the FAT writes of a resize live beside `read_fat_table`, but are
// not part of the public API.
pub(crate) use operations::{copy_fat_to_other_copies, zero_fat_extension};

pub use operations::{
    bootstrap_sector_size, count_free_clusters, open_fat32_device, read_backup_boot_sector,
    read_boot_sector, read_boot_sector_for_recovery, read_fat_table, read_fsinfo,
    write_backup_boot_sector, write_boot_sector, write_fsinfo,
};

// Re-export validation
pub use validation::{
    boot_sectors_match, validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};
