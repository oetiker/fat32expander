//! Shared fixtures for the resize module's unit tests.
//!
//! [F-R42] split these tests across four files; `test_boot_sector` and
//! `temp_device` are used by more than one of them, so they live here
//! rather than being duplicated per module.

use crate::device::Device;
use crate::fat32::BootSector;
use tempfile::NamedTempFile;

/// Build a FAT32 boot sector with a standard geometry.
pub(crate) fn test_boot_sector(total_sectors: u32, fat_size: u32) -> BootSector {
    let mut data = vec![0u8; 512];
    data[0] = 0xEB;
    data[1] = 0x58;
    data[2] = 0x90;
    data[11..13].copy_from_slice(&512u16.to_le_bytes());
    data[13] = 8; // sectors per cluster
    data[14..16].copy_from_slice(&32u16.to_le_bytes()); // reserved
    data[16] = 2; // FATs
    data[21] = 0xF8;
    data[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    data[36..40].copy_from_slice(&fat_size.to_le_bytes());
    data[44..48].copy_from_slice(&2u32.to_le_bytes());
    data[48] = 0x01;
    data[50] = 0x06;
    data[510] = 0x55;
    data[511] = 0xAA;
    BootSector::from_bytes(&data).unwrap()
}

/// Build a FAT32 boot sector with a standard geometry at a given sector size.
///
/// [F-R23]'s safety net needs a filesystem whose sector size is not the 512
/// that `Device::open` assumes, so the geometry the device believes in can be
/// checked against the geometry the filesystem declares.
pub(crate) fn test_boot_sector_with_sector_size(
    total_sectors: u32,
    fat_size: u32,
    sector_size: u16,
) -> BootSector {
    let mut data = vec![0u8; sector_size as usize];
    data[0] = 0xEB;
    data[1] = 0x58;
    data[2] = 0x90;
    data[11..13].copy_from_slice(&sector_size.to_le_bytes());
    data[13] = 8; // sectors per cluster
    data[14..16].copy_from_slice(&32u16.to_le_bytes()); // reserved
    data[16] = 2; // FATs
    data[21] = 0xF8;
    data[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    data[36..40].copy_from_slice(&fat_size.to_le_bytes());
    data[44..48].copy_from_slice(&2u32.to_le_bytes());
    data[48] = 0x01;
    data[50] = 0x06;
    data[510] = 0x55;
    data[511] = 0xAA;
    BootSector::from_bytes(&data).unwrap()
}

/// A temp-file-backed device of `sectors` zeroed 512-byte sectors.
pub(crate) fn temp_device(sectors: u64) -> (NamedTempFile, Device) {
    temp_device_with_sector_size(sectors, 512)
}

/// A temp-file-backed device of `sectors` zeroed sectors of `sector_size`
/// bytes, with the device already addressing them at that size.
pub(crate) fn temp_device_with_sector_size(
    sectors: u64,
    sector_size: u32,
) -> (NamedTempFile, Device) {
    let file = NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        vec![0u8; sectors as usize * sector_size as usize],
    )
    .unwrap();
    let mut device = Device::open(file.path()).unwrap();
    device.set_sector_size(sector_size);
    (file, device)
}
