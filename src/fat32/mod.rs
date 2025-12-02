pub mod operations;
pub mod structs;
pub mod validation;

// Re-export types from structs
pub use structs::{BootSector, ClusterId, FSInfo, SectorNum, fat_entry};

// Re-export operations
pub use operations::{
    count_free_clusters, find_free_cluster, read_backup_boot_sector, read_boot_sector,
    read_boot_sector_for_recovery, read_cluster, read_fat_entry, read_fat_table, read_fsinfo,
    write_backup_boot_sector, write_boot_sector, write_cluster, write_fat_entries, write_fat_entry,
    write_fat_entry_with_size, write_fsinfo,
};

// Re-export validation
pub use validation::{
    boot_sectors_match, validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};
