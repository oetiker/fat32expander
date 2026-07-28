//! Read-only query of a FAT32 filesystem.
//!
//! Split out of `executor.rs` for [F-R42]: inspecting a filesystem has
//! nothing to do with resizing one, and bundling them meant a read-only
//! query shared a file with the code that rewrites the boot sector.

use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::{boot_sectors_match, read_backup_boot_sector, read_boot_sector, read_fsinfo};

/// Get information about a FAT32 filesystem without modifying it
pub fn get_fs_info(device_path: impl AsRef<std::path::Path>) -> Result<FSInfoReport> {
    let device_path = device_path.as_ref();
    let mut device = Device::open_readonly(device_path)?;
    let boot = read_boot_sector(&mut device)?;

    let backup_sector = boot.backup_boot_sector();
    let backup_boot = read_backup_boot_sector(&device, backup_sector)?;
    let backup_matches = boot_sectors_match(&boot, &backup_boot);

    let fsinfo_sector = boot.fs_info_sector();
    let fsinfo = read_fsinfo(&device, fsinfo_sector)?;

    let device_sectors = device.total_sectors();
    let current_sectors = boot.total_sectors();
    let can_grow = device_sectors > current_sectors as u64;

    let max_new_size = if can_grow {
        Some(device_sectors * boot.bytes_per_sector() as u64)
    } else {
        None
    };

    Ok(FSInfoReport {
        device_path: device_path.to_path_buf(),
        bytes_per_sector: boot.bytes_per_sector(),
        sectors_per_cluster: boot.sectors_per_cluster(),
        reserved_sectors: boot.reserved_sectors(),
        num_fats: boot.num_fats(),
        fat_size_sectors: boot.fat_size(),
        total_sectors: boot.total_sectors(),
        // get_fs_info reads through the strict validator, which rejects a layout
        // that does not fit - so this cannot be None by the time we are here.
        data_clusters: boot.data_clusters().ok_or_else(|| {
            Error::BootSectorValidation(format!(
                "Filesystem layout does not fit in {} sectors",
                boot.total_sectors()
            ))
        })?,
        root_cluster: boot.root_cluster(),
        fsinfo_sector: boot.fs_info_sector(),
        backup_boot_sector: boot.backup_boot_sector(),
        free_clusters: fsinfo.free_count(),
        backup_matches,
        device_sectors,
        can_grow,
        current_size_bytes: current_sectors as u64 * boot.bytes_per_sector() as u64,
        max_new_size_bytes: max_new_size,
    })
}

/// Report about a FAT32 filesystem
#[derive(Debug)]
pub struct FSInfoReport {
    pub device_path: std::path::PathBuf,
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub fat_size_sectors: u32,
    pub total_sectors: u32,
    pub data_clusters: u32,
    pub root_cluster: u32,
    pub fsinfo_sector: u16,
    pub backup_boot_sector: u16,
    pub free_clusters: u32,
    pub backup_matches: bool,
    pub device_sectors: u64,
    pub can_grow: bool,
    pub current_size_bytes: u64,
    pub max_new_size_bytes: Option<u64>,
}
