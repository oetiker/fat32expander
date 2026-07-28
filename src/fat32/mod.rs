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
pub use operations::{
    bootstrap_sector_size, count_free_clusters, find_free_cluster, open_fat32_device,
    read_backup_boot_sector, read_boot_sector, read_boot_sector_for_recovery, read_cluster,
    read_fat_entry, read_fat_table, read_fsinfo, write_backup_boot_sector, write_boot_sector,
    write_cluster, write_fat_entries, write_fat_entry, write_fat_entry_with_size, write_fsinfo,
};

// Re-export validation
pub use validation::{
    boot_sectors_match, validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};
