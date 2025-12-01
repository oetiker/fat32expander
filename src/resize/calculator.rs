use crate::error::{Error, Result};
use crate::fat32::BootSector;

/// Result of size calculations for a resize operation
#[derive(Debug, Clone)]
pub struct SizeCalculation {
    /// Original total sectors in the filesystem
    pub old_total_sectors: u32,
    /// New total sectors after resize
    pub new_total_sectors: u32,
    /// Original FAT size in sectors
    pub old_fat_size: u32,
    /// New FAT size in sectors
    pub new_fat_size: u32,
    /// Number of data clusters after resize
    pub new_data_clusters: u32,
    /// Number of free clusters after resize
    pub new_free_clusters: u32,
    /// Whether FAT tables need to grow
    pub fat_needs_growth: bool,
    /// Number of sectors the FAT grows by
    pub fat_growth_sectors: u32,
    /// First cluster that would be overwritten if FAT grows
    pub first_affected_cluster: u32,
    /// Last cluster that would be overwritten if FAT grows
    pub last_affected_cluster: u32,
}

impl SizeCalculation {
    /// Size increase in bytes
    pub fn size_increase(&self, bytes_per_sector: u16) -> u64 {
        let increase_sectors = self.new_total_sectors - self.old_total_sectors;
        increase_sectors as u64 * bytes_per_sector as u64
    }

    /// New filesystem size in bytes
    pub fn new_size_bytes(&self, bytes_per_sector: u16) -> u64 {
        self.new_total_sectors as u64 * bytes_per_sector as u64
    }

    /// Additional clusters gained
    pub fn additional_clusters(&self, old_data_clusters: u32) -> u32 {
        self.new_data_clusters.saturating_sub(old_data_clusters)
    }
}

/// Calculate the new size parameters for a resize operation
pub fn calculate_new_size(boot: &BootSector, device_sectors: u64) -> Result<SizeCalculation> {
    let old_total_sectors = boot.total_sectors();
    let old_fat_size = boot.fat_size();
    let old_data_clusters = boot.data_clusters();

    // New total sectors is the device size (in sectors)
    // Cap at u32::MAX since FAT32 uses 32-bit sector counts
    let new_total_sectors = if device_sectors > u32::MAX as u64 {
        return Err(Error::Calculation(format!(
            "Device size {} sectors exceeds FAT32 maximum",
            device_sectors
        )));
    } else {
        device_sectors as u32
    };

    // Check for shrink (not supported)
    if new_total_sectors < old_total_sectors {
        return Err(Error::ShrinkNotSupported);
    }

    // Check if already at max size
    if new_total_sectors == old_total_sectors {
        return Err(Error::AlreadyMaxSize);
    }

    // Calculate new FAT size
    let new_fat_size = calculate_fat_size(
        new_total_sectors,
        boot.reserved_sectors(),
        boot.num_fats(),
        boot.sectors_per_cluster(),
        boot.bytes_per_sector(),
    )?;

    // Calculate new data clusters
    let new_data_sectors = new_total_sectors
        - boot.reserved_sectors() as u32
        - (boot.num_fats() as u32 * new_fat_size);
    let new_data_clusters = new_data_sectors / boot.sectors_per_cluster() as u32;

    // Verify we still have FAT32 (>= 65525 clusters)
    if new_data_clusters < 65525 {
        return Err(Error::Calculation(format!(
            "New cluster count {} would not be FAT32",
            new_data_clusters
        )));
    }

    // Check if FAT needs to grow
    let fat_needs_growth = new_fat_size > old_fat_size;
    let fat_growth_sectors = new_fat_size.saturating_sub(old_fat_size);

    // Calculate which clusters would be affected by FAT growth
    let (first_affected_cluster, last_affected_cluster) = if fat_needs_growth {
        // FAT growth affects the data area right after the current FAT tables
        // The first data sector moves forward by (fat_growth_sectors * num_fats)
        let growth_per_fat = fat_growth_sectors;
        let total_growth = growth_per_fat * boot.num_fats() as u32;

        // These are the clusters that currently occupy the space where
        // the new FAT sectors will go
        let sectors_per_cluster = boot.sectors_per_cluster() as u32;

        // Number of clusters that will be overwritten
        let affected_clusters = total_growth.div_ceil(sectors_per_cluster);

        // First affected cluster is cluster 2 (the first data cluster)
        let first = 2u32;
        let last = first + affected_clusters - 1;

        (first, last)
    } else {
        (0, 0)
    };

    // Calculate free clusters (will be updated based on actual FAT contents)
    // This is an estimate; actual value depends on current usage
    let additional_clusters = new_data_clusters.saturating_sub(old_data_clusters);
    let new_free_clusters = additional_clusters; // Placeholder; actual = old_free + additional

    Ok(SizeCalculation {
        old_total_sectors,
        new_total_sectors,
        old_fat_size,
        new_fat_size,
        new_data_clusters,
        new_free_clusters,
        fat_needs_growth,
        fat_growth_sectors,
        first_affected_cluster,
        last_affected_cluster,
    })
}

/// Calculate the required FAT size in sectors
///
/// This uses the algorithm from the Microsoft FAT specification.
/// The result may be slightly larger than strictly necessary, but never too small.
pub fn calculate_fat_size(
    total_sectors: u32,
    reserved_sectors: u16,
    num_fats: u8,
    sectors_per_cluster: u8,
    bytes_per_sector: u16,
) -> Result<u32> {
    // From Microsoft FAT specification:
    // RootDirSectors = ((BPB_RootEntCnt * 32) + (BPB_BytsPerSec – 1)) / BPB_BytsPerSec
    // For FAT32, RootDirSectors = 0

    // TmpVal1 = DskSize – (BPB_ResvdSecCnt + RootDirSectors)
    let tmp_val1 = total_sectors as u64 - reserved_sectors as u64;

    // TmpVal2 = (256 * BPB_SecPerClus) + BPB_NumFATs
    // For FAT32: TmpVal2 = TmpVal2 / 2
    let entries_per_sector = bytes_per_sector as u64 / 4; // 4 bytes per FAT32 entry
    let tmp_val2 = (entries_per_sector * sectors_per_cluster as u64) + (num_fats as u64 / 2);

    if tmp_val2 == 0 {
        return Err(Error::Calculation(
            "Division by zero in FAT size calculation".to_string(),
        ));
    }

    // FATSz = (TMPVal1 + (TmpVal2 – 1)) / TmpVal2
    let fat_size = tmp_val1.div_ceil(tmp_val2);

    // Ensure it fits in u32
    if fat_size > u32::MAX as u64 {
        return Err(Error::Calculation(
            "FAT size exceeds u32 maximum".to_string(),
        ));
    }

    Ok(fat_size as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_boot_sector(total_sectors: u32, fat_size: u32) -> BootSector {
        let mut data = [0u8; 512];

        // Jump instruction
        data[0] = 0xEB;
        data[1] = 0x58;
        data[2] = 0x90;

        // Bytes per sector (512)
        data[11] = 0x00;
        data[12] = 0x02;

        // Sectors per cluster (8)
        data[13] = 0x08;

        // Reserved sectors (32)
        data[14] = 0x20;
        data[15] = 0x00;

        // Number of FATs (2)
        data[16] = 0x02;

        // Media type
        data[21] = 0xF8;

        // Total sectors 32
        data[32..36].copy_from_slice(&total_sectors.to_le_bytes());

        // FAT size 32
        data[36..40].copy_from_slice(&fat_size.to_le_bytes());

        // Root cluster (2)
        data[44..48].copy_from_slice(&2u32.to_le_bytes());

        // FSInfo sector (1)
        data[48] = 0x01;

        // Backup boot sector (6)
        data[50] = 0x06;

        // Boot signature
        data[510] = 0x55;
        data[511] = 0xAA;

        BootSector::from_bytes(&data).unwrap()
    }

    #[test]
    fn test_calculate_fat_size() {
        // Test with known values
        // 1GB volume with 512 byte sectors and 8 sectors per cluster
        let total_sectors = 2_097_152u32; // 1GB
        let fat_size = calculate_fat_size(total_sectors, 32, 2, 8, 512).unwrap();

        // FAT32 needs 4 bytes per entry
        // With 8 sectors per cluster, we have ~262000 clusters
        // At 4 bytes each, we need ~1MB of FAT = ~2048 sectors
        assert!(fat_size > 0);
        assert!(fat_size < 10000); // Sanity check
    }

    #[test]
    fn test_calculate_new_size_growth() {
        // Start with a 500MB filesystem
        let boot = create_test_boot_sector(1_000_000, 1000);

        // Grow to 2GB
        let calc = calculate_new_size(&boot, 4_000_000).unwrap();

        assert_eq!(calc.old_total_sectors, 1_000_000);
        assert_eq!(calc.new_total_sectors, 4_000_000);
        assert!(calc.new_fat_size >= calc.old_fat_size);
    }

    #[test]
    fn test_calculate_new_size_no_shrink() {
        let boot = create_test_boot_sector(2_000_000, 2000);

        // Try to shrink - should fail
        let result = calculate_new_size(&boot, 1_000_000);
        assert!(matches!(result, Err(Error::ShrinkNotSupported)));
    }

    #[test]
    fn test_calculate_new_size_same_size() {
        let boot = create_test_boot_sector(2_000_000, 2000);

        // Same size - should fail
        let result = calculate_new_size(&boot, 2_000_000);
        assert!(matches!(result, Err(Error::AlreadyMaxSize)));
    }
}
