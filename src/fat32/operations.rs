use crate::device::{Device, PROBE_BYTES};
use crate::error::{Error, Result};
use crate::fat32::structs::{fat_entry, BootSector, FSInfo};
use crate::fat32::validation::{
    validate_boot_sector, validate_boot_sector_for_recovery, validate_fsinfo,
};
use std::path::Path;

// The open-time probe must see a whole boot sector at every size FAT32
// permits, or `bootstrap_sector_size` would parse a truncated one.
const _: () = {
    let sizes = BootSector::VALID_SECTOR_SIZES;
    let mut i = 0;
    while i < sizes.len() {
        assert!(sizes[i] as usize <= PROBE_BYTES);
        i += 1;
    }
};

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
    if !BootSector::VALID_SECTOR_SIZES.contains(&sector_size) {
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
    // The device addresses sectors at the size this boot sector declared when
    // it was opened ([`bootstrap_sector_size`] checked it then), so sector 0
    // is exactly the boot sector. A device opened at some other size is
    // refused rather than parsed from a misaddressed read ([F-22]).
    let boot = BootSector::from_bytes(&device.read_sector(0)?)?;
    let declared = boot.bytes_per_sector() as u32;
    if declared != device.sector_size() {
        return Err(Error::BootSectorValidation(format!(
            "Boot sector declares {}-byte sectors, but the device is addressed with {}-byte sectors",
            declared,
            device.sector_size()
        )));
    }
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

/// First sector of FAT copy `fat_number` when every copy is `fat_size`
/// sectors long.
///
/// [F-15]: the one place that knows the FAT layout. The size is an argument
/// because a resize lays the copies out at the new FAT size while `boot`
/// still declares the old one.
pub(crate) fn fat_copy_start(boot: &BootSector, fat_number: u8, fat_size: u32) -> u64 {
    boot.first_fat_sector() + fat_number as u64 * fat_size as u64
}

/// Zero FAT1's growth, sectors `old_fat_size..new_fat_size`, so every new
/// entry reads as a free cluster. Does nothing unless the FAT grows.
///
/// This must be called AFTER relocation, and the order is not a preference.
/// The sectors this zeroes are physically the sectors relocation has just
/// vacated and moved cluster data out of. Running it first would zero the
/// region while it still holds live cluster data, and relocation would then
/// copy that data forward from an area this function had already erased.
///
/// [F-R16]: this doc said the exact opposite ("must be called BEFORE
/// relocation so that reading new FAT sectors returns valid data") while the
/// call site had it right, and the correct `copy_fat_to_other_copies` doc
/// below made the wrong one easy to trust. Nothing enforces the order but
/// the call site in `resize_fat32` ([F-R15]).
pub(crate) fn zero_fat_extension(
    device: &Device,
    boot: &BootSector,
    old_fat_size: u32,
    new_fat_size: u32,
) -> Result<()> {
    if new_fat_size <= old_fat_size {
        return Ok(());
    }

    let fat1_start = fat_copy_start(boot, 0, new_fat_size);
    let free_sector = vec![0u8; boot.bytes_per_sector() as usize];
    for sector_offset in old_fat_size..new_fat_size {
        device.write_sector(fat1_start + sector_offset as u64, &free_sector)?;
    }

    Ok(())
}

/// Copy FAT1, `new_fat_size` sectors, over every other FAT copy, each laid
/// out at `new_fat_size`. Does nothing unless the FAT grows.
///
/// This must be called AFTER relocation and after [`zero_fat_extension`], so
/// the copies get FAT1 as it now stands.
pub(crate) fn copy_fat_to_other_copies(
    device: &Device,
    boot: &BootSector,
    old_fat_size: u32,
    new_fat_size: u32,
) -> Result<()> {
    if new_fat_size <= old_fat_size {
        return Ok(());
    }

    let fat1_start = fat_copy_start(boot, 0, new_fat_size);
    let chunk_sectors = fat_chunk_sectors(boot);
    for fat_number in 1..boot.num_fats() {
        let dest_start = fat_copy_start(boot, fat_number, new_fat_size);
        let mut offset = 0u32;
        while offset < new_fat_size {
            let count = std::cmp::min(chunk_sectors, new_fat_size - offset);
            let data = device.read_sectors(fat1_start + offset as u64, count)?;
            device.write_sectors(dest_start + offset as u64, &data)?;
            offset += count;
        }
    }

    Ok(())
}

/// How much of a FAT is read or copied per device call.
///
/// [F-18]: a byte count, so the memory held is the same at every sector size;
/// the old `256` sectors was 128 KiB at 512-byte sectors but 1 MiB at 4096.
const FAT_CHUNK_BYTES: u32 = 128 * 1024;

/// Sectors per FAT chunk at `boot`'s sector size, at least one.
fn fat_chunk_sectors(boot: &BootSector) -> u32 {
    (FAT_CHUNK_BYTES / boot.bytes_per_sector() as u32).max(1)
}

/// Read the entire FAT table (one copy)
///
/// Entries are decoded chunk by chunk, so the FAT is held once, as entries,
/// not a second time as raw bytes ([F-18]).
pub fn read_fat_table(device: &Device, boot: &BootSector, fat_number: u8) -> Result<Vec<u32>> {
    let fat_start = fat_copy_start(boot, fat_number, boot.fat_size());
    let fat_sectors = boot.fat_size();
    let chunk_sectors = fat_chunk_sectors(boot);

    let entry_count = fat_sectors as usize * boot.bytes_per_sector() as usize / 4;
    let mut entries = Vec::with_capacity(entry_count);

    let mut sector = 0u32;
    while sector < fat_sectors {
        let count = std::cmp::min(chunk_sectors, fat_sectors - sector);
        let data = device.read_sectors(fat_start + sector as u64, count)?;
        entries.extend(
            data.chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        );
        sector += count;
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

    /// [F-15]: the one formula for where FAT copy `n` starts, in sectors.
    /// The resize asks with a FAT size that is not yet in the boot sector,
    /// so the size is an argument rather than read from `boot`.
    #[test]
    fn fat_copy_start_counts_whole_fats_from_the_reserved_area() {
        let boot = |reserved: u16| {
            let mut data = vec![0u8; 512];
            data[11..13].copy_from_slice(&512u16.to_le_bytes());
            data[14..16].copy_from_slice(&reserved.to_le_bytes());
            data[16] = 2;
            data[510] = 0x55;
            data[511] = 0xAA;
            BootSector::from_bytes(&data).unwrap()
        };
        assert_eq!(fat_copy_start(&boot(32), 0, 1000), 32);
        assert_eq!(fat_copy_start(&boot(32), 1, 1000), 1032);
        assert_eq!(fat_copy_start(&boot(7), 1, 3), 10);
        // Large enough that a 32-bit product would wrap.
        assert_eq!(fat_copy_start(&boot(32), 1, u32::MAX), 32 + u32::MAX as u64);
    }

    /// Safety net for [F-22]. The probe that addresses the device must accept
    /// every sector size FAT32 permits -- 1024 and 2048 included, though the
    /// error text has only ever named 512 and 4096 -- and reject the rest as
    /// `UnsupportedSectorSize`, carrying the size it found.
    #[test]
    fn bootstrap_accepts_exactly_the_four_fat32_sector_sizes() {
        let leading = |bps: u16| {
            let mut data = vec![0u8; 4096];
            data[11..13].copy_from_slice(&bps.to_le_bytes());
            data
        };
        for bps in [512u16, 1024, 2048, 4096] {
            assert_eq!(
                bootstrap_sector_size(&leading(bps)).unwrap(),
                bps as u32,
                "bytes_per_sector={bps}"
            );
        }
        for bps in [0u16, 256, 511, 513, 3072, 8192] {
            assert!(
                matches!(
                    bootstrap_sector_size(&leading(bps)),
                    Err(Error::UnsupportedSectorSize(n)) if n == bps as u32
                ),
                "bytes_per_sector={bps} was not rejected as unsupported"
            );
        }
    }

    /// [F-22]: the boot sector is read as sector 0 of the device, so a device
    /// addressed at a size other than the one the boot sector declares is
    /// refused instead of parsed from a misaddressed read.
    #[test]
    fn a_boot_sector_is_read_only_at_the_size_it_declares() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0u8; 64 * 4096];
        data[11..13].copy_from_slice(&4096u16.to_le_bytes());
        std::fs::write(file.path(), &data).unwrap();

        let matching = Device::open_with_sector_size(file.path(), 4096).unwrap();
        let boot = read_boot_sector_unvalidated(&matching).unwrap();
        assert_eq!(boot.bytes_per_sector(), 4096);

        let mismatched = Device::open_with_sector_size(file.path(), 512).unwrap();
        match read_boot_sector_unvalidated(&mismatched) {
            Err(Error::BootSectorValidation(msg)) => {
                assert!(msg.contains("4096") && msg.contains("512"), "{msg}")
            }
            other => panic!("expected BootSectorValidation, got {other:?}"),
        }
    }

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
