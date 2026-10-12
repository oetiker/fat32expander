//! Read-only query of a FAT32 filesystem.
//!
//! Split out of `executor.rs` for [F-R42]: inspecting a filesystem has
//! nothing to do with resizing one, and bundling them meant a read-only
//! query shared a file with the code that rewrites the boot sector.

use crate::error::{Error, Result};
use crate::fat32::{
    boot_sectors_match, open_fat32_device, read_backup_boot_sector, read_boot_sector, read_fsinfo,
    BootSector,
};
use crate::resize::calculator::calculate_new_size;

/// Get information about a FAT32 filesystem without modifying it
pub fn get_fs_info(device_path: impl AsRef<std::path::Path>) -> Result<FSInfoReport> {
    let device_path = device_path.as_ref();
    let device = open_fat32_device(device_path, false)?;
    let boot = read_boot_sector(&device)?;

    let backup_sector = boot.backup_boot_sector();
    let backup_boot = read_backup_boot_sector(&device, backup_sector)?;
    let backup_matches = boot_sectors_match(&boot, &backup_boot);

    let fsinfo_sector = boot.fs_info_sector();
    let fsinfo = read_fsinfo(&device, fsinfo_sector)?;

    // Checked once, here, so every later reader gets a plain `u32` instead of
    // an `Option` it would be tempted to unwrap. `read_boot_sector` goes
    // through the strict validator, which rejects a layout that does not fit,
    // so this is unreachable in practice - but it is the kind of "cannot
    // happen" that [F-R7] showed can happen on a crafted image.
    let data_clusters = boot.data_clusters().ok_or_else(|| {
        Error::BootSectorValidation(format!(
            "Filesystem layout does not fit in {} sectors",
            boot.total_sectors()
        ))
    })?;

    Ok(FSInfoReport {
        device_path: device_path.to_path_buf(),
        boot,
        data_clusters,
        free_clusters: fsinfo.free_count(),
        backup_matches,
        device_sectors: device.total_sectors(),
    })
}

/// Report about a FAT32 filesystem.
///
/// [F-R43]: this deliberately does **not** mirror `BootSector` field by field.
/// It used to carry 17 public fields, 14 of them straight `boot.*()` copies,
/// restated in the struct definition, the construction block and the renderer -
/// so surfacing one more filesystem field meant three or four coordinated
/// edits, and a copy that drifted was invisible.
///
/// Now it holds the boot sector itself and exposes it through [`Self::boot`].
/// Geometry questions go to `BootSector`, which owns them; this type adds only
/// what it actually knows that the boot sector does not - where the filesystem
/// was read from, how large the *device* is, and the answers derived from
/// comparing the two. Surfacing another geometry field now costs no edits here
/// at all.
#[derive(Debug)]
pub struct FSInfoReport {
    device_path: std::path::PathBuf,
    boot: BootSector,
    data_clusters: u32,
    free_clusters: u32,
    backup_matches: bool,
    device_sectors: u64,
}

impl FSInfoReport {
    /// The boot sector this report describes.
    ///
    /// All geometry - sector size, cluster size, FAT count and size, total
    /// sectors, root cluster, FSInfo and backup sector numbers - is read from
    /// here rather than copied out.
    pub fn boot(&self) -> &BootSector {
        &self.boot
    }

    /// Path the filesystem was read from.
    pub fn device_path(&self) -> &std::path::Path {
        &self.device_path
    }

    /// Number of data clusters, validated at construction.
    ///
    /// Derived from the geometry rather than stored on the boot sector, and
    /// checked once in `get_fs_info`, so callers get a `u32` and not an
    /// `Option` inviting an `unwrap`.
    pub fn data_clusters(&self) -> u32 {
        self.data_clusters
    }

    /// Free cluster count as recorded in the FSInfo sector.
    ///
    /// This is a *hint* maintained by whichever driver last wrote the volume,
    /// not authoritative truth - the FAT is ([F-R45]). `FSInfo::UNKNOWN_FREE`
    /// means the hint is absent.
    pub fn free_clusters(&self) -> u32 {
        self.free_clusters
    }

    /// Whether the backup boot sector agrees with the primary.
    pub fn backup_matches(&self) -> bool {
        self.backup_matches
    }

    /// Size of the underlying device, in sectors.
    ///
    /// This is the number that makes the report more than a boot-sector dump:
    /// the difference between it and `boot().total_sectors()` is the headroom a
    /// resize would claim.
    pub fn device_sectors(&self) -> u64 {
        self.device_sectors
    }

    /// Whether `resize` would grow the filesystem onto this device.
    ///
    /// A larger device is not enough: the resize also refuses when the volume
    /// cannot gain a cluster ("Nothing to do") or would end with more clusters
    /// than FAT32 allows. `info` used to say "Can grow: Yes" in both cases, so
    /// this asks the same calculation the resize makes.
    pub fn can_grow(&self) -> bool {
        calculate_new_size(&self.boot, self.device_sectors).is_ok()
    }

    /// Current filesystem size in bytes.
    pub fn current_size_bytes(&self) -> u64 {
        self.boot.total_sectors() as u64 * self.boot.bytes_per_sector() as u64
    }

    /// Size in bytes the filesystem could be grown to, if it can grow at all.
    pub fn max_new_size_bytes(&self) -> Option<u64> {
        if self.can_grow() {
            Some(self.device_sectors * self.boot.bytes_per_sector() as u64)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize::testutil::test_boot_sector;

    fn report(boot: BootSector, device_sectors: u64) -> FSInfoReport {
        FSInfoReport {
            device_path: "test.img".into(),
            data_clusters: boot.data_clusters().unwrap(),
            boot,
            free_clusters: 0,
            backup_matches: true,
            device_sectors,
        }
    }

    /// A FAT with no free entry, on a volume of one-sector clusters.
    fn full_fat_boot() -> BootSector {
        // 782 sectors x 128 entries = 100,096 entries: room for 100,094.
        let boot = test_boot_sector(32 + 2 * 782 + 100_094, 782);
        let mut data = boot.as_bytes().to_vec();
        data[13] = 1; // sectors per cluster
        BootSector::from_bytes(&data).unwrap()
    }

    /// `resize` reports "Nothing to do" here: growing the full FAT by one
    /// sector costs two clusters, more than one extra sector adds. `info`
    /// must not offer a growth the resize will not make.
    #[test]
    fn a_full_fat_that_cannot_gain_a_cluster_cannot_grow() {
        let boot = full_fat_boot();
        let device_sectors = boot.total_sectors() as u64 + 1;
        let info = report(boot, device_sectors);

        assert!(!info.can_grow());
        assert_eq!(info.max_new_size_bytes(), None);
    }

    /// `resize` refuses growth past the FAT32 cluster ceiling (exit 13), so
    /// `info` must not offer it.
    #[test]
    fn growth_past_the_fat32_cluster_ceiling_cannot_grow() {
        let info = report(test_boot_sector(1_000_000, 1000), u32::MAX as u64);

        assert!(!info.can_grow());
        assert_eq!(info.max_new_size_bytes(), None);
    }
}
