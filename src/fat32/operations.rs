use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::structs::{fat_entry, BootSector, FSInfo};
use crate::fat32::validation::{
    validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};
use std::path::Path;

/// Maximum sector size supported by FAT32 (4096 bytes)
const MAX_SECTOR_SIZE: usize = 4096;

/// Valid FAT32 sector sizes
const VALID_SECTOR_SIZES: &[u16] = &[512, 1024, 2048, 4096];

/// Determine the sector size a FAT32 boot sector declares.
///
/// This is the probe [`Device::open_with`] needs: it turns leading bytes into
/// the sector size the device must be addressed with, so the geometry is known
/// before any sector-addressed I/O is possible ([F-R23]). It deliberately
/// validates nothing beyond the sector-size field -- a device whose boot sector
/// is otherwise damaged still has to be *addressable* for recovery to read it.
pub fn bootstrap_sector_size(leading_bytes: &[u8]) -> Result<u32> {
    let boot = BootSector::from_bytes(leading_bytes)?;
    let sector_size = boot.bytes_per_sector();
    if !VALID_SECTOR_SIZES.contains(&sector_size) {
        return Err(Error::UnsupportedSectorSize(sector_size as u32));
    }
    Ok(sector_size as u32)
}

/// Open a device for FAT32 access, with its geometry already established.
///
/// [F-R23]/[F-R40]: opening and learning the sector size used to be two steps,
/// with the second performed as a side effect of a function named `read_...`.
/// They are one step now, and this is it.
pub fn open_fat32_device<P: AsRef<Path>>(path: P, writable: bool) -> Result<Device> {
    Device::open_with(path, writable, bootstrap_sector_size)
}

/// Read and parse the boot sector from a device
///
/// The device already addresses sectors at the size its boot sector declares --
/// [`open_fat32_device`] established that before the device existed. This is a
/// pure read: it does not reconfigure the device ([F-R40]).
pub fn read_boot_sector(device: &Device) -> Result<BootSector> {
    let boot = read_boot_sector_unvalidated(device)?;
    validate_boot_sector(&boot)?;
    Ok(boot)
}

/// Read and parse the boot sector, allowing invalidated signature for recovery
///
/// This is used when trying to recover from an interrupted resize operation
/// where the boot sector signature was intentionally invalidated.
pub fn read_boot_sector_for_recovery(device: &Device) -> Result<BootSector> {
    let boot = read_boot_sector_unvalidated(device)?;
    validate_boot_sector_for_recovery(&boot)?;
    Ok(boot)
}

/// The parsing both readers share, without either one's validation policy.
fn read_boot_sector_unvalidated(device: &Device) -> Result<BootSector> {
    // Read max sector size bytes to ensure we have the complete boot sector
    let data = device.read_bytes_at(0, MAX_SECTOR_SIZE)?;

    // Parse the boot sector (validates minimum 512 bytes)
    let boot = BootSector::from_bytes(&data)?;

    // Validate the sector size is a valid FAT32 size
    let sector_size = boot.bytes_per_sector();
    if !VALID_SECTOR_SIZES.contains(&sector_size) {
        return Err(Error::UnsupportedSectorSize(sector_size as u32));
    }

    // Re-parse with the correct sector size (trimmed to actual size)
    BootSector::from_bytes(&data[..sector_size as usize])
}

/// Read and parse the backup boot sector
pub fn read_backup_boot_sector(device: &Device, sector: u16) -> Result<BootSector> {
    let data = device.read_sector(sector as u64)?;
    BootSector::from_bytes(&data)
}

/// Read and parse the FSInfo sector
pub fn read_fsinfo(device: &Device, sector: u16) -> Result<FSInfo> {
    let data = device.read_sector(sector as u64)?;
    let fsinfo = FSInfo::from_bytes(&data)?;
    validate_fsinfo(&fsinfo)?;
    Ok(fsinfo)
}

/// Write boot sector to device
pub fn write_boot_sector(device: &Device, boot: &BootSector) -> Result<()> {
    device.write_sector(0, boot.as_bytes())
}

/// Write backup boot sector to device
pub fn write_backup_boot_sector(device: &Device, boot: &BootSector, sector: u16) -> Result<()> {
    device.write_sector(sector as u64, boot.as_bytes())
}

/// Write FSInfo sector to device
pub fn write_fsinfo(device: &Device, fsinfo: &FSInfo, sector: u16) -> Result<()> {
    device.write_sector(sector as u64, fsinfo.as_bytes())
}

/// Read the entire FAT table (one copy)
pub fn read_fat_table(device: &Device, boot: &BootSector, fat_number: u8) -> Result<Vec<u32>> {
    let fat_start = boot.first_fat_sector() + (fat_number as u64 * boot.fat_size() as u64);
    let fat_sectors = boot.fat_size();
    let bytes_per_sector = boot.bytes_per_sector() as usize;

    let total_bytes = fat_sectors as usize * bytes_per_sector;
    let mut fat_data = Vec::with_capacity(total_bytes);

    // Read FAT in chunks
    const CHUNK_SECTORS: u32 = 256; // Read 128KB at a time
    let mut sector = 0u32;
    while sector < fat_sectors {
        let count = std::cmp::min(CHUNK_SECTORS, fat_sectors - sector);
        let data = device.read_sectors(fat_start + sector as u64, count)?;
        fat_data.extend_from_slice(&data);
        sector += count;
    }

    // Convert bytes to u32 entries (little-endian)
    let entry_count = total_bytes / 4;
    let mut entries = Vec::with_capacity(entry_count);
    for i in 0..entry_count {
        let offset = i * 4;
        let entry = u32::from_le_bytes([
            fat_data[offset],
            fat_data[offset + 1],
            fat_data[offset + 2],
            fat_data[offset + 3],
        ]);
        entries.push(entry);
    }

    Ok(entries)
}

/// Count free clusters in the FAT
///
/// [F-R34] Kept by explicit decision while its seven dead neighbours were
/// deleted. It has no production caller today -- only its own unit test -- but
/// it is the function [F-R45] names as the way to reconcile the FSInfo free
/// count against the FAT, which is the authoritative source. Deleting it would
/// have foreclosed that fix, and unlike the helpers removed alongside it this
/// is a coherent operation on a FAT a library consumer can already read via
/// [`read_fat_table`], rather than a fragment of the abandoned
/// cluster-renumbering design.
///
/// Being re-exported keeps it exempt from the dead-code lint, so it will not
/// announce itself if [F-R45] is never done.
pub fn count_free_clusters(fat: &[u32], max_cluster: u32) -> u32 {
    let end = std::cmp::min(fat.len(), (max_cluster + 2) as usize);
    fat[2..end]
        .iter()
        .filter(|&&e| fat_entry::is_free(e))
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_free_clusters() {
        let fat = vec![
            0x0FFFFFF8, // Entry 0: Media type
            0x0FFFFFFF, // Entry 1: Reserved
            0x00000003, // Entry 2: Points to cluster 3
            0x0FFFFFFF, // Entry 3: End of chain
            0x00000000, // Entry 4: Free
            0x00000000, // Entry 5: Free
            0x00000007, // Entry 6: Points to cluster 7
            0x0FFFFFFF, // Entry 7: End of chain
        ];

        // Max cluster 5 means we only count up to entry 7 (5+2)
        assert_eq!(count_free_clusters(&fat, 5), 2); // Entries 4 and 5 are free
    }

    // [F-R34] `test_find_free_cluster` was deleted with `find_free_cluster`.
    // It never called the function: it reimplemented the search inline with
    // iterators and asserted on that, so it would have kept passing after the
    // deletion while covering nothing. Keeping it would have left a test whose
    // name claims coverage the suite does not have.
}
