use crate::error::{Error, Result};
use crate::fat32::structs::{BootSector, FSInfo};

/// Validate a boot sector to ensure it's a valid FAT32 filesystem
pub fn validate_boot_sector(boot: &BootSector) -> Result<()> {
    validate_boot_sector_impl(boot, false)
}

/// Validate a boot sector, optionally allowing invalidated signature (for recovery)
///
/// When `allow_invalidated` is true, accepts boot signature of 0x0000 which
/// indicates an interrupted resize operation that invalidated the boot sector.
pub fn validate_boot_sector_for_recovery(boot: &BootSector) -> Result<()> {
    validate_boot_sector_impl(boot, true)
}

fn validate_boot_sector_impl(boot: &BootSector, allow_invalidated: bool) -> Result<()> {
    // Check boot signature (must be 0xAA55, or 0x0000 if recovery mode)
    let sig = boot.boot_signature();
    if sig != 0xAA55 {
        if allow_invalidated && sig == 0x0000 {
            // Invalidated boot sector - allowed in recovery mode
        } else {
            return Err(Error::BootSectorValidation(format!(
                "Invalid boot signature: {:#06X} (expected 0xAA55)",
                sig
            )));
        }
    }

    // Check bytes per sector (must be 512, 1024, 2048, or 4096)
    let bps = boot.bytes_per_sector();
    if !matches!(bps, 512 | 1024 | 2048 | 4096) {
        return Err(Error::BootSectorValidation(format!(
            "Invalid bytes per sector: {} (must be 512, 1024, 2048, or 4096)",
            bps
        )));
    }

    // Check sectors per cluster (must be power of 2, 1-128)
    let spc = boot.sectors_per_cluster();
    if spc == 0 || !spc.is_power_of_two() || spc > 128 {
        return Err(Error::BootSectorValidation(format!(
            "Invalid sectors per cluster: {} (must be power of 2, 1-128)",
            spc
        )));
    }

    // Check reserved sectors (must be >= 1)
    if boot.reserved_sectors() == 0 {
        return Err(Error::BootSectorValidation(
            "Reserved sector count is 0".to_string(),
        ));
    }

    // Check number of FATs (typically 2, but 1 is allowed)
    let num_fats = boot.num_fats();
    if num_fats == 0 || num_fats > 2 {
        return Err(Error::BootSectorValidation(format!(
            "Invalid number of FATs: {} (must be 1 or 2)",
            num_fats
        )));
    }

    // For FAT32, root entry count must be 0
    if boot.root_entry_count() != 0 {
        return Err(Error::InvalidFAT32(
            "Root entry count is non-zero (not FAT32)".to_string(),
        ));
    }

    // For FAT32, total sectors 16 should be 0
    if boot.total_sectors_16() != 0 {
        return Err(Error::InvalidFAT32(
            "Total sectors 16 is non-zero (not FAT32)".to_string(),
        ));
    }

    // For FAT32, fat size 16 should be 0
    if boot.fat_size_16() != 0 {
        return Err(Error::InvalidFAT32(
            "FAT size 16 is non-zero (not FAT32)".to_string(),
        ));
    }

    // Check total sectors
    if boot.total_sectors_32() == 0 {
        return Err(Error::BootSectorValidation(
            "Total sectors is 0".to_string(),
        ));
    }

    // Check FAT size
    if boot.fat_size_32() == 0 {
        return Err(Error::BootSectorValidation(
            "FAT size is 0".to_string(),
        ));
    }

    // Check root cluster (must be >= 2)
    if boot.root_cluster() < 2 {
        return Err(Error::BootSectorValidation(format!(
            "Invalid root cluster: {} (must be >= 2)",
            boot.root_cluster()
        )));
    }

    // Check media type (should be 0xF0 or 0xF8-0xFF)
    let media = boot.media_type();
    if media != 0xF0 && !(0xF8..=0xFF).contains(&media) {
        return Err(Error::BootSectorValidation(format!(
            "Invalid media type: {:#04X} (expected 0xF0 or 0xF8-0xFF)",
            media
        )));
    }

    // Verify this is actually FAT32 by cluster count
    let cluster_count = boot.data_clusters();
    if cluster_count < 65525 {
        return Err(Error::InvalidFAT32(format!(
            "Cluster count {} indicates FAT12/16, not FAT32 (need >= 65525)",
            cluster_count
        )));
    }

    // Check FS type string (optional but recommended)
    let fs_type = boot.fs_type();
    if !fs_type.starts_with(b"FAT32") && !fs_type.starts_with(b"FAT") {
        // This is a warning-level issue, not an error
        // Some tools don't set this correctly
    }

    Ok(())
}

/// Validate FSInfo sector
pub fn validate_fsinfo(fsinfo: &FSInfo) -> Result<()> {
    // Check lead signature
    if fsinfo.lead_sig() != FSInfo::LEAD_SIG {
        return Err(Error::FSInfoValidation(format!(
            "Invalid lead signature: {:#010X} (expected {:#010X})",
            fsinfo.lead_sig(),
            FSInfo::LEAD_SIG
        )));
    }

    // Check structure signature
    if fsinfo.struc_sig() != FSInfo::STRUC_SIG {
        return Err(Error::FSInfoValidation(format!(
            "Invalid structure signature: {:#010X} (expected {:#010X})",
            fsinfo.struc_sig(),
            FSInfo::STRUC_SIG
        )));
    }

    // Check trail signature
    if fsinfo.trail_sig() != FSInfo::TRAIL_SIG {
        return Err(Error::FSInfoValidation(format!(
            "Invalid trail signature: {:#010X} (expected {:#010X})",
            fsinfo.trail_sig(),
            FSInfo::TRAIL_SIG
        )));
    }

    Ok(())
}

/// Compare two boot sectors for essential equality
/// (ignores fields that may differ like volume serial number)
pub fn boot_sectors_match(primary: &BootSector, backup: &BootSector) -> bool {
    // Compare critical fields
    primary.bytes_per_sector() == backup.bytes_per_sector()
        && primary.sectors_per_cluster() == backup.sectors_per_cluster()
        && primary.reserved_sectors() == backup.reserved_sectors()
        && primary.num_fats() == backup.num_fats()
        && primary.total_sectors_32() == backup.total_sectors_32()
        && primary.fat_size_32() == backup.fat_size_32()
        && primary.root_cluster() == backup.root_cluster()
        && primary.fs_info_sector() == backup.fs_info_sector()
        && primary.backup_boot_sector() == backup.backup_boot_sector()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_valid_fat32_boot_sector() -> [u8; 512] {
        let mut data = [0u8; 512];

        // Jump instruction
        data[0] = 0xEB;
        data[1] = 0x58;
        data[2] = 0x90;

        // OEM name
        data[3..11].copy_from_slice(b"MSDOS5.0");

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

        // Root entry count = 0 (FAT32)
        data[17] = 0x00;
        data[18] = 0x00;

        // Total sectors 16 = 0 (FAT32)
        data[19] = 0x00;
        data[20] = 0x00;

        // Media type (0xF8 = hard disk)
        data[21] = 0xF8;

        // FAT size 16 = 0 (FAT32)
        data[22] = 0x00;
        data[23] = 0x00;

        // Total sectors 32 (2000000 - large enough for FAT32)
        let total_sectors: u32 = 2_000_000;
        data[32..36].copy_from_slice(&total_sectors.to_le_bytes());

        // FAT size 32 (7813)
        let fat_size: u32 = 7813;
        data[36..40].copy_from_slice(&fat_size.to_le_bytes());

        // Root cluster (2)
        data[44..48].copy_from_slice(&2u32.to_le_bytes());

        // FSInfo sector (1)
        data[48] = 0x01;

        // Backup boot sector (6)
        data[50] = 0x06;

        // FS type string
        data[82..90].copy_from_slice(b"FAT32   ");

        // Boot signature
        data[510] = 0x55;
        data[511] = 0xAA;

        data
    }

    #[test]
    fn test_valid_boot_sector() {
        let data = create_valid_fat32_boot_sector();
        let boot = BootSector::from_bytes(&data).unwrap();
        assert!(validate_boot_sector(&boot).is_ok());
    }

    #[test]
    fn test_invalid_boot_signature() {
        let mut data = create_valid_fat32_boot_sector();
        data[510] = 0x00; // Invalid signature
        let boot = BootSector::from_bytes(&data).unwrap();
        let result = validate_boot_sector(&boot);
        assert!(matches!(result, Err(Error::BootSectorValidation(_))));
    }

    #[test]
    fn test_not_fat32() {
        let mut data = create_valid_fat32_boot_sector();
        // Set root entry count to non-zero (FAT12/16)
        data[17] = 0x00;
        data[18] = 0x02;
        let boot = BootSector::from_bytes(&data).unwrap();
        let result = validate_boot_sector(&boot);
        assert!(matches!(result, Err(Error::InvalidFAT32(_))));
    }

    #[test]
    fn test_valid_fsinfo() {
        let mut data = [0u8; 512];
        data[0..4].copy_from_slice(&FSInfo::LEAD_SIG.to_le_bytes());
        data[484..488].copy_from_slice(&FSInfo::STRUC_SIG.to_le_bytes());
        data[508..512].copy_from_slice(&FSInfo::TRAIL_SIG.to_le_bytes());

        let fsinfo = FSInfo::from_bytes(&data).unwrap();
        assert!(validate_fsinfo(&fsinfo).is_ok());
    }

    #[test]
    fn test_invalid_fsinfo() {
        let data = [0u8; 512]; // All zeros - invalid signatures
        let fsinfo = FSInfo::from_bytes(&data).unwrap();
        let result = validate_fsinfo(&fsinfo);
        assert!(matches!(result, Err(Error::FSInfoValidation(_))));
    }
}
