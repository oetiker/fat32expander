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
pub use structs::{fat_entry, BootSector, ClusterId, FSInfo, SectorNum};

// Re-export operations
//
// [F-R34] Seven entry-level and cluster-level helpers were removed from here
// and from `operations.rs`: they were leftovers from the pre-refactor
// cluster-renumbering design, which the current data-shift approach does not
// need, and being `pub` in a library exempted them from the dead-code lint.
// `count_free_clusters` was deliberately kept -- see the note on it.
pub use operations::{
    bootstrap_sector_size, count_free_clusters, open_fat32_device, read_backup_boot_sector,
    read_boot_sector, read_boot_sector_for_recovery, read_fat_table, read_fsinfo,
    write_backup_boot_sector, write_boot_sector, write_fsinfo,
};

// Re-export validation
pub use validation::{
    boot_sectors_match, validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};
