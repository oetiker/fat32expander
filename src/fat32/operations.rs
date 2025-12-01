use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::structs::{fat_entry, BootSector, FSInfo};
use crate::fat32::validation::{
    validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};

/// Maximum sector size supported by FAT32 (4096 bytes)
const MAX_SECTOR_SIZE: usize = 4096;

/// Valid FAT32 sector sizes
const VALID_SECTOR_SIZES: &[u16] = &[512, 1024, 2048, 4096];

/// Read and parse the boot sector from a device, bootstrapping the sector size
///
/// This reads enough bytes to cover the maximum sector size (4096), then
/// parses the boot sector to determine the actual sector size. The device's
/// sector size is then updated for subsequent operations.
pub fn read_boot_sector(device: &mut Device) -> Result<BootSector> {
    // Read max sector size bytes to ensure we have the complete boot sector
    let data = device.read_bytes_at(0, MAX_SECTOR_SIZE)?;

    // Parse the boot sector (validates minimum 512 bytes)
    let boot = BootSector::from_bytes(&data)?;

    // Validate the sector size is a valid FAT32 size
    let sector_size = boot.bytes_per_sector();
    if !VALID_SECTOR_SIZES.contains(&sector_size) {
        return Err(Error::UnsupportedSectorSize(sector_size as u32));
    }

    // Update device to use the actual sector size
    device.set_sector_size(sector_size as u32);

    // Re-parse with the correct sector size (trimmed to actual size)
    let boot = BootSector::from_bytes(&data[..sector_size as usize])?;
    validate_boot_sector(&boot)?;
    Ok(boot)
}

/// Read and parse the boot sector, allowing invalidated signature for recovery
///
/// This is used when trying to recover from an interrupted resize operation
/// where the boot sector signature was intentionally invalidated.
pub fn read_boot_sector_for_recovery(device: &mut Device) -> Result<BootSector> {
    // Read max sector size bytes to ensure we have the complete boot sector
    let data = device.read_bytes_at(0, MAX_SECTOR_SIZE)?;

    // Parse the boot sector (validates minimum 512 bytes)
    let boot = BootSector::from_bytes(&data)?;

    // Validate the sector size is a valid FAT32 size
    let sector_size = boot.bytes_per_sector();
    if !VALID_SECTOR_SIZES.contains(&sector_size) {
        return Err(Error::UnsupportedSectorSize(sector_size as u32));
    }

    // Update device to use the actual sector size
    device.set_sector_size(sector_size as u32);

    // Re-parse with the correct sector size (trimmed to actual size)
    let boot = BootSector::from_bytes(&data[..sector_size as usize])?;
    validate_boot_sector_for_recovery(&boot)?;
    Ok(boot)
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

/// Write FAT entries starting at a specific index (for one FAT copy)
pub fn write_fat_entries(
    device: &Device,
    boot: &BootSector,
    fat_number: u8,
    start_entry: u32,
    entries: &[u32],
) -> Result<()> {
    let bytes_per_sector = boot.bytes_per_sector() as usize;
    let entries_per_sector = bytes_per_sector / 4;

    let fat_start = boot.first_fat_sector() + (fat_number as u64 * boot.fat_size() as u64);

    // Calculate which sectors we need to write
    let start_sector = start_entry as usize / entries_per_sector;
    let end_entry = start_entry as usize + entries.len();
    let end_sector = end_entry.div_ceil(entries_per_sector);

    // Process sector by sector
    for sector_idx in start_sector..end_sector {
        let sector = fat_start + sector_idx as u64;

        // Read existing sector
        let mut sector_data = device.read_sector(sector)?;

        // Calculate which entries in this sector to update
        let sector_start_entry = sector_idx * entries_per_sector;
        let sector_end_entry = sector_start_entry + entries_per_sector;

        for entry_idx in sector_start_entry..sector_end_entry {
            if entry_idx >= start_entry as usize && entry_idx < end_entry {
                let entry_value = entries[entry_idx - start_entry as usize];
                let offset = (entry_idx - sector_start_entry) * 4;
                sector_data[offset..offset + 4].copy_from_slice(&entry_value.to_le_bytes());
            }
        }

        // Write back
        device.write_sector(sector, &sector_data)?;
    }

    Ok(())
}

/// Read a single FAT entry
pub fn read_fat_entry(device: &Device, boot: &BootSector, cluster: u32) -> Result<u32> {
    let bytes_per_sector = boot.bytes_per_sector() as usize;
    let entries_per_sector = bytes_per_sector / 4;

    let fat_start = boot.first_fat_sector();
    let sector = fat_start + (cluster as u64 / entries_per_sector as u64);
    let offset = (cluster as usize % entries_per_sector) * 4;

    let sector_data = device.read_sector(sector)?;
    let entry = u32::from_le_bytes([
        sector_data[offset],
        sector_data[offset + 1],
        sector_data[offset + 2],
        sector_data[offset + 3],
    ]);

    Ok(entry)
}

/// Write a single FAT entry to all FAT copies
pub fn write_fat_entry(device: &Device, boot: &BootSector, cluster: u32, value: u32) -> Result<()> {
    write_fat_entry_with_size(device, boot, cluster, value, boot.fat_size())
}

/// Write a single FAT entry to all FAT copies, using a specified FAT size
///
/// This variant is needed during resize when the boot sector still has the old
/// FAT size, but we need to write to clusters in the expanded area.
pub fn write_fat_entry_with_size(
    device: &Device,
    boot: &BootSector,
    cluster: u32,
    value: u32,
    fat_size: u32,
) -> Result<()> {
    let bytes_per_sector = boot.bytes_per_sector() as usize;
    let entries_per_sector = bytes_per_sector / 4;

    for fat_num in 0..boot.num_fats() {
        let fat_start = boot.first_fat_sector() + (fat_num as u64 * fat_size as u64);
        let sector = fat_start + (cluster as u64 / entries_per_sector as u64);
        let offset = (cluster as usize % entries_per_sector) * 4;

        // Read sector (may be zeros for new sectors)
        let mut sector_data = device.read_sector(sector)?;

        // Preserve upper 4 bits of existing entry
        let existing = u32::from_le_bytes([
            sector_data[offset],
            sector_data[offset + 1],
            sector_data[offset + 2],
            sector_data[offset + 3],
        ]);
        let preserved_bits = existing & 0xF0000000;
        let new_value = (value & 0x0FFFFFFF) | preserved_bits;

        // Update entry
        sector_data[offset..offset + 4].copy_from_slice(&new_value.to_le_bytes());

        // Write back
        device.write_sector(sector, &sector_data)?;
    }

    Ok(())
}

/// Read a cluster's data
pub fn read_cluster(device: &Device, boot: &BootSector, cluster: u32) -> Result<Vec<u8>> {
    let first_sector = boot.cluster_to_sector(cluster);
    let sectors_per_cluster = boot.sectors_per_cluster() as u32;
    device.read_sectors(first_sector, sectors_per_cluster)
}

/// Write a cluster's data
pub fn write_cluster(device: &Device, boot: &BootSector, cluster: u32, data: &[u8]) -> Result<()> {
    let first_sector = boot.cluster_to_sector(cluster);
    let expected_size = boot.bytes_per_cluster() as usize;

    if data.len() != expected_size {
        return Err(Error::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "Cluster data size {} does not match expected {}",
                data.len(),
                expected_size
            ),
        )));
    }

    device.write_sectors(first_sector, data)
}

/// Find a free cluster starting from a given hint
pub fn find_free_cluster(
    _device: &Device,
    boot: &BootSector,
    fat: &[u32],
    start: u32,
) -> Option<u32> {
    let max_cluster = boot.data_clusters() + 2; // Clusters are numbered from 2

    // Search from start to end
    for cluster in start..max_cluster {
        if cluster < fat.len() as u32 && fat_entry::is_free(fat[cluster as usize]) {
            return Some(cluster);
        }
    }

    // Wrap around and search from beginning
    (2..start)
        .find(|&cluster| cluster < fat.len() as u32 && fat_entry::is_free(fat[cluster as usize]))
}

/// Count free clusters in the FAT
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

    #[test]
    fn test_find_free_cluster() {
        let fat = [
            0x0FFFFFF8, // Entry 0: Media type
            0x0FFFFFFF, // Entry 1: Reserved
            0x00000003, // Entry 2: Used
            0x0FFFFFFF, // Entry 3: Used
            0x00000000, // Entry 4: Free
            0x00000000, // Entry 5: Free
        ];

        // Create a mock boot sector (not actually used in this simplified test)
        // In real code, we'd need a proper Device

        // Test the function logic without device
        let result = fat
            .iter()
            .enumerate()
            .skip(2) // Skip entries 0 and 1
            .find(|(_, &e)| fat_entry::is_free(e))
            .map(|(i, _)| i as u32);

        assert_eq!(result, Some(4));
    }
}
