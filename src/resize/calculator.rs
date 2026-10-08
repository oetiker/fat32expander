use crate::error::{Error, Result};
use crate::fat32::BootSector;
use crate::resize::checkpoint::{ResizeCheckpoint, ResizePhase};

/// The geometry a resize plans, and the numbers derived from it.
///
/// [F-09]: crate-private. It used to be public through
/// `ResizeResult::calculation`, which kept members no code read out of
/// reach of the dead-code lint, among them a "free clusters after resize"
/// that held a placeholder on a fresh run and 0 on a resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SizeCalculation {
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
    /// Whether FAT tables need to grow
    pub fat_needs_growth: bool,
    /// Number of sectors the FAT grows by
    pub fat_growth_sectors: u32,
    /// First cluster that would be overwritten if FAT grows
    pub first_affected_cluster: u32,
}

impl SizeCalculation {
    /// The calculation for `old -> new` geometry on the filesystem `boot`
    /// describes, or `None` if the new layout does not fit.
    fn for_geometry(
        boot: &BootSector,
        old_total_sectors: u32,
        new_total_sectors: u32,
        old_fat_size: u32,
        new_fat_size: u32,
    ) -> Option<Self> {
        let new_data_clusters = boot
            .with_geometry(new_total_sectors, new_fat_size)
            .data_clusters()?;
        let (first_affected_cluster, _) = affected_cluster_range(boot, old_fat_size, new_fat_size);
        Some(Self {
            old_total_sectors,
            new_total_sectors,
            old_fat_size,
            new_fat_size,
            new_data_clusters,
            fat_needs_growth: new_fat_size > old_fat_size,
            fat_growth_sectors: new_fat_size.saturating_sub(old_fat_size),
            first_affected_cluster,
        })
    }

    /// The calculation an interrupted run made, rebuilt from its checkpoint.
    ///
    /// A checkpoint carries the geometry a previous run committed to. If that
    /// geometry does not describe a filesystem that fits, the record is not
    /// one we can resume from.
    pub(crate) fn from_checkpoint(boot: &BootSector, cp: &ResizeCheckpoint) -> Result<Self> {
        Self::for_geometry(
            boot,
            cp.old_total_sectors,
            cp.new_total_sectors,
            cp.old_fat_size,
            cp.new_fat_size,
        )
        .ok_or(Error::CheckpointGeometryUnusable {
            new_total_sectors: cp.new_total_sectors,
            new_fat_size: cp.new_fat_size,
        })
    }

    /// The checkpoint recording this calculation at `phase`; the inverse of
    /// [`from_checkpoint`](Self::from_checkpoint).
    pub(crate) fn checkpoint(&self, phase: ResizePhase, volume_id: u32) -> ResizeCheckpoint {
        ResizeCheckpoint::new(
            phase,
            volume_id,
            self.old_total_sectors,
            self.new_total_sectors,
            self.old_fat_size,
            self.new_fat_size,
        )
    }
}

/// Calculate the new size parameters for a resize operation
pub(crate) fn calculate_new_size(
    boot: &BootSector,
    device_sectors: u64,
) -> Result<SizeCalculation> {
    let old_total_sectors = boot.total_sectors();
    let old_fat_size = boot.fat_size();
    // The boot sector reaching here has passed validation, which rejects a
    // layout that does not fit - but this function is public, so it checks
    // rather than assuming.
    boot.data_clusters().ok_or_else(|| {
        Error::Calculation(format!(
            "Current filesystem layout does not fit in its own {} sectors",
            boot.total_sectors()
        ))
    })?;

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

    // The device is smaller than the filesystem says it is. This is not a
    // shrink request: the only caller passes the device's actual size and the
    // tool offers no way to ask for a smaller one, so reaching here means a
    // truncated image, the wrong device, or a corrupt `total_sectors` field
    // ([F-R33]).
    if new_total_sectors < old_total_sectors {
        return Err(Error::DeviceTooSmall {
            current: new_total_sectors as u64,
            minimum: old_total_sectors as u64,
        });
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

    // Calculate the new layout, from the geometry the volume will actually have
    let calculation = SizeCalculation::for_geometry(
        boot,
        old_total_sectors,
        new_total_sectors,
        old_fat_size,
        new_fat_size,
    )
    .ok_or_else(|| {
        Error::Calculation(format!(
            "Projected layout does not fit: {} sectors with a FAT of {} sectors",
            new_total_sectors, new_fat_size
        ))
    })?;

    // Verify the projected volume is still FAT32
    if calculation.new_data_clusters < BootSector::MIN_FAT32_CLUSTERS {
        return Err(Error::Calculation(format!(
            "New cluster count {} would not be FAT32",
            calculation.new_data_clusters
        )));
    }

    Ok(calculation)
}

/// The range of clusters that currently sit where the grown FAT tables will go
///
/// The data area begins at cluster 2, so growing the FAT by `total_growth`
/// sectors overwrites the clusters spanning those sectors - rounded up, since a
/// partially overwritten cluster is still lost. Returns `(0, 0)` when the FAT
/// does not grow.
///
/// This is the count of clusters *overwritten*. It is deliberately not the
/// distance the data area moves: that distance is `num_fats * fat growth`
/// exactly, and is read off the boot sector by the relocator rather than
/// recomputed here.
pub(crate) fn affected_cluster_range(
    boot: &BootSector,
    old_fat_size: u32,
    new_fat_size: u32,
) -> (u32, u32) {
    if new_fat_size <= old_fat_size {
        return (0, 0);
    }

    // [F-02]: on a resume these sizes come from a checkpoint record, so the
    // arithmetic saturates rather than trusting them not to overflow.
    let total_growth = (new_fat_size - old_fat_size).saturating_mul(boot.num_fats() as u32);
    let affected_clusters = total_growth.div_ceil(boot.sectors_per_cluster().max(1) as u32);

    let first = 2u32;
    (first, first.saturating_add(affected_clusters - 1))
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
    use crate::resize::checkpoint::{ResizeCheckpoint, ResizePhase};

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

    /// Pins the exact layout numbers a resize is planned from. `new_data_clusters`
    /// and the affected-cluster range are consumed by the relocator, so a silent
    /// change here moves user data.
    #[test]
    fn test_calculate_new_size_layout_values() {
        // 512 B sectors, 8 sectors/cluster, 32 reserved, 2 FATs.
        let boot = create_test_boot_sector(1_000_000, 1000);

        let calc = calculate_new_size(&boot, 4_000_000).unwrap();

        assert_eq!(calc.new_fat_size, 3903);
        assert_eq!(calc.fat_growth_sectors, 2903);
        assert_eq!(calc.new_data_clusters, 499_020);
        assert!(calc.fat_needs_growth);
        // 2 FATs x 2903 sectors = 5806 sectors of growth, spanning 726 clusters.
        assert_eq!(calc.first_affected_cluster, 2);
        assert_eq!(
            affected_cluster_range(&boot, 1000, calc.new_fat_size).1,
            727
        );
    }

    /// [F-09]: the checkpoint is written from the calculation in one place,
    /// and carries all four of its geometry numbers.
    #[test]
    fn a_checkpoint_carries_the_geometry_of_its_calculation() {
        let boot = create_test_boot_sector(1_000_000, 1000);
        let calc = calculate_new_size(&boot, 4_000_000).unwrap();

        let cp = calc.checkpoint(ResizePhase::DataCopied, 0xCAFE);

        assert_eq!(cp.phase, ResizePhase::DataCopied);
        assert_eq!(cp.volume_id, Some(0xCAFE));
        assert_eq!(cp.old_total_sectors, calc.old_total_sectors);
        assert_eq!(cp.new_total_sectors, calc.new_total_sectors);
        assert_eq!(cp.old_fat_size, calc.old_fat_size);
        assert_eq!(cp.new_fat_size, calc.new_fat_size);
    }

    /// [F-09]: a resume rebuilds its calculation from the checkpoint. It must
    /// be the calculation the interrupted run made -- a resume used to carry a
    /// free count of 0 where a fresh run carried a placeholder.
    #[test]
    fn a_resume_rebuilds_the_calculation_the_fresh_run_made() {
        let boot = create_test_boot_sector(1_000_000, 1000);
        let calc = calculate_new_size(&boot, 4_000_000).unwrap();
        let cp = calc.checkpoint(ResizePhase::DataCopied, 1);

        assert_eq!(SizeCalculation::from_checkpoint(&boot, &cp).unwrap(), calc);
    }

    #[test]
    fn a_checkpoint_whose_geometry_does_not_fit_is_unusable() {
        let boot = create_test_boot_sector(1_000_000, 1000);
        let cp = ResizeCheckpoint::new(ResizePhase::Started, 1, 1_000_000, 1_000, 1000, 900);

        assert!(matches!(
            SizeCalculation::from_checkpoint(&boot, &cp),
            Err(Error::CheckpointGeometryUnusable {
                new_total_sectors: 1_000,
                new_fat_size: 900,
            })
        ));
    }

    /// The calculator's data-cluster arithmetic must agree with the canonical
    /// accessor on `BootSector` for the geometry it is planning against.
    #[test]
    fn test_new_data_clusters_agrees_with_boot_sector_accessor() {
        let boot = create_test_boot_sector(1_000_000, 1000);
        let calc = calculate_new_size(&boot, 4_000_000).unwrap();

        let mut grown = boot.clone();
        grown.set_total_sectors_32(calc.new_total_sectors);
        grown.set_fat_size_32(calc.new_fat_size);

        assert_eq!(Some(calc.new_data_clusters), grown.data_clusters());
    }

    #[test]
    fn test_calculate_new_size_no_shrink() {
        let boot = create_test_boot_sector(2_000_000, 2000);

        // A device smaller than the filesystem must fail. [F-R33]: this is not
        // a user asking to shrink -- the only caller passes the device's actual
        // size, and there is no shrink command -- so it is reported as the
        // device being too small, and the message names both numbers.
        let result = calculate_new_size(&boot, 1_000_000);
        assert!(
            matches!(
                result,
                Err(Error::DeviceTooSmall {
                    current: 1_000_000,
                    minimum: 2_000_000
                })
            ),
            "expected DeviceTooSmall carrying both sizes, got: {result:?}"
        );
    }

    /// [F-R33]: the message must tell the user what is actually wrong. "Cannot
    /// shrink filesystem (not supported)" describes an operation this tool does
    /// not offer and the user did not request; a truncated image or the wrong
    /// device is what really produces this.
    #[test]
    fn a_device_smaller_than_the_filesystem_reports_both_sizes() {
        let boot = create_test_boot_sector(2_000_000, 2000);
        let err = calculate_new_size(&boot, 1_000_000).expect_err("must fail");
        let msg = err.to_string();

        assert!(
            msg.contains("1000000") && msg.contains("2000000"),
            "the message must name the device size and the size needed, got: {msg}"
        );
        assert!(
            !msg.contains("shrink"),
            "the user did not ask to shrink, got: {msg}"
        );
    }

    /// Safety net for [F-R52]. The second bare `65525` lives here, guarding
    /// the *projected* layout rather than the current one, and nothing
    /// exercised it: every other calculator test grows a volume that is
    /// comfortably FAT32 both before and after.
    #[test]
    fn a_projected_layout_below_the_fat32_floor_is_rejected() {
        // 8 sectors per cluster, so even the grown volume yields ~12,400
        // clusters -- a growth that succeeds arithmetically but would leave
        // behind something that is not a FAT32 filesystem.
        let boot = create_test_boot_sector(50_000, 100);
        let result = calculate_new_size(&boot, 100_000);
        let msg = match result {
            Err(Error::Calculation(msg)) => msg,
            other => panic!("expected a Calculation error, got {other:?}"),
        };
        assert!(
            msg.contains("would not be FAT32"),
            "rejected for the wrong reason: {msg}"
        );
    }

    #[test]
    fn test_calculate_new_size_same_size() {
        let boot = create_test_boot_sector(2_000_000, 2000);

        // Same size - should fail
        let result = calculate_new_size(&boot, 2_000_000);
        assert!(matches!(result, Err(Error::AlreadyMaxSize)));
    }
}
