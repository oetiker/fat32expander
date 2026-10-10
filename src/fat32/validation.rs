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
    // Check boot signature (must be 0xAA55, or 0x0000 if recovery mode).
    //
    // A zeroed signature is this tool's own in-progress marker, so it gets its
    // own error variant rather than a formatted message: the CLI's decision to
    // attempt crash recovery keys on it, and keying on message text made
    // rewording either side silently disable recovery ([F-R5]).
    let sig = boot.boot_signature();
    if sig != BootSector::VALID_SIGNATURE {
        if sig == 0x0000 {
            if !allow_invalidated {
                return Err(Error::BootSectorInvalidated);
            }
            // Invalidated boot sector - allowed in recovery mode
        } else {
            return Err(Error::BootSectorValidation(format!(
                "Invalid boot signature: {:#06X} (expected {:#06X})",
                sig,
                BootSector::VALID_SIGNATURE
            )));
        }
    }

    // Check bytes per sector (must be one of the four FAT32 permits)
    let bps = boot.bytes_per_sector();
    if !BootSector::VALID_SECTOR_SIZES.contains(&bps) {
        return Err(Error::BootSectorValidation(format!(
            "Invalid bytes per sector: {} (must be one of {:?})",
            bps,
            BootSector::VALID_SECTOR_SIZES
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

    // [F-10]: FSInfo and the backup boot sector must lie in the reserved area.
    // The resize writes a boot sector to the backup location and FSInfo to
    // its own, so a corrupt pointer would otherwise aim those writes at a FAT
    // or data sector. Backup 0 means "no backup"; FSInfo is always read, so
    // it has no such value.
    let reserved = boot.reserved_sectors();
    let fs_info = boot.fs_info_sector();
    if fs_info == 0 || fs_info >= reserved {
        return Err(Error::BootSectorValidation(format!(
            "Invalid FSInfo sector: {} (must be within the {} reserved sectors, after sector 0)",
            fs_info, reserved
        )));
    }
    let backup = boot.backup_boot_sector();
    if backup >= reserved || (backup != 0 && backup == fs_info) {
        return Err(Error::BootSectorValidation(format!(
            "Invalid backup boot sector: {} (must be 0 or within the {} reserved sectors, \
             apart from FSInfo at sector {})",
            backup, reserved, fs_info
        )));
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
        return Err(Error::BootSectorValidation("FAT size is 0".to_string()));
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

    // Cross-field check: the reserved area plus the FATs must fit inside the
    // volume the boot sector declares. This has to come before the cluster-count
    // test below, which consumes the same arithmetic - when the layout does not
    // fit there is no cluster count to speak of, and reporting the failure as
    // "looks like FAT12/16" sends the reader somewhere unrelated.
    // `data_clusters` is also None for a zero cluster size, but that is already
    // rejected above, so reaching here can only mean the layout does not fit.
    let Some(cluster_count) = boot.data_clusters() else {
        return Err(Error::BootSectorValidation(format!(
            "Filesystem layout does not fit in the volume: {} reserved sectors + \
             {} FATs x {} sectors exceeds {} total sectors",
            boot.reserved_sectors(),
            boot.num_fats(),
            boot.fat_size(),
            boot.total_sectors()
        )));
    };

    // Verify this is actually FAT32 by cluster count
    if cluster_count < BootSector::MIN_FAT32_CLUSTERS {
        return Err(Error::InvalidFAT32(format!(
            "Cluster count {} indicates FAT12/16, not FAT32 (need >= {})",
            cluster_count,
            BootSector::MIN_FAT32_CLUSTERS
        )));
    }
    if cluster_count > BootSector::MAX_FAT32_CLUSTERS {
        return Err(Error::InvalidFAT32(format!(
            "Cluster count {} exceeds the FAT32 maximum of {}",
            cluster_count,
            BootSector::MAX_FAT32_CLUSTERS
        )));
    }

    // The FS type label at offset 82 is not checked: the cluster count above
    // decides the FAT type, and formatters write all sorts of labels there.

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

    /// Build a boot sector that is a legitimate FAT32 volume under the given
    /// geometry, satisfying every check `validate_boot_sector_impl` makes.
    fn valid_boot_sector_with(
        bytes_per_sector: u16,
        sectors_per_cluster: u8,
        reserved_sectors: u16,
        num_fats: u8,
        total_sectors: u32,
        fat_size: u32,
    ) -> BootSector {
        let mut data = [0u8; 512];
        data[0] = 0xEB;
        data[1] = 0x58;
        data[2] = 0x90;
        data[3..11].copy_from_slice(b"MSDOS5.0");
        data[11..13].copy_from_slice(&bytes_per_sector.to_le_bytes());
        data[13] = sectors_per_cluster;
        data[14..16].copy_from_slice(&reserved_sectors.to_le_bytes());
        data[16] = num_fats;
        data[21] = 0xF8;
        data[32..36].copy_from_slice(&total_sectors.to_le_bytes());
        data[36..40].copy_from_slice(&fat_size.to_le_bytes());
        data[44..48].copy_from_slice(&2u32.to_le_bytes());
        data[48] = 0x01;
        data[50] = 0x06;
        data[82..90].copy_from_slice(b"FAT32   ");
        data[510] = 0x55;
        data[511] = 0xAA;
        BootSector::from_bytes(&data).unwrap()
    }

    /// Validation must keep accepting legitimate filesystems across all four
    /// supported sector sizes and a range of cluster sizes. This is the guard
    /// against a tightened validator starting to reject real volumes: the
    /// pre-existing `test_valid_boot_sector` covers one geometry only
    /// (512-byte sectors, 8 sectors per cluster).
    #[test]
    fn validation_accepts_legitimate_geometries() {
        let cases: [(u16, u8, u16, u8, u32, u32); 7] = [
            (512, 1, 32, 2, 1_000_000, 7813),
            (512, 8, 32, 2, 2_000_000, 7813),
            (512, 128, 32, 2, 60_000_000, 3700),
            (1024, 4, 32, 2, 1_200_000, 1200),
            (2048, 2, 32, 1, 800_000, 900),
            (4096, 1, 32, 2, 500_000, 500),
            // Single-FAT volume with a large reserved area.
            (512, 4, 4096, 1, 3_000_000, 6000),
        ];

        for (bps, spc, reserved, num_fats, total, fat_size) in cases {
            let boot = valid_boot_sector_with(bps, spc, reserved, num_fats, total, fat_size);
            let label =
                format!("bps={bps} spc={spc} reserved={reserved} nfats={num_fats} total={total}");
            assert!(
                boot.data_clusters().is_some_and(|c| c >= 65525),
                "case is not FAT32, fix the fixture: {label}"
            );
            assert!(
                validate_boot_sector(&boot).is_ok(),
                "validation rejected a legitimate filesystem: {label} -> {:?}",
                validate_boot_sector(&boot)
            );
        }
    }

    /// Safety net for [F-T02]. The cases above stop at a few million clusters.
    /// A ceiling on the cluster count must still accept volumes with hundreds
    /// of millions of clusters, at small and large sector and cluster sizes.
    #[test]
    fn validation_accepts_volumes_with_hundreds_of_millions_of_clusters() {
        // (bytes per sector, sectors per cluster, FATs, data clusters)
        let cases: [(u16, u8, u8, u32); 4] = [
            (512, 1, 2, 250_000_000),
            (4096, 1, 2, 250_000_000),
            (512, 8, 1, 250_000_000),
            (512, 128, 2, 33_000_000),
        ];

        for (bps, spc, num_fats, clusters) in cases {
            let fat_size = ((clusters as u64 + 2) * 4).div_ceil(bps as u64) as u32;
            let total = 32 + num_fats as u64 * fat_size as u64 + clusters as u64 * spc as u64;
            let label = format!("bps={bps} spc={spc} nfats={num_fats} clusters={clusters}");
            assert!(total <= u32::MAX as u64, "fixture overflows: {label}");
            let boot = valid_boot_sector_with(bps, spc, 32, num_fats, total as u32, fat_size);
            assert_eq!(boot.data_clusters(), Some(clusters), "fixture: {label}");
            assert!(
                validate_boot_sector(&boot).is_ok(),
                "validation rejected a legitimate filesystem: {label} -> {:?}",
                validate_boot_sector(&boot)
            );
        }
    }

    /// Safety net for [F-10]. Every other fixture puts FSInfo at sector 1 and
    /// the backup boot sector at 6. A validator that starts checking those
    /// pointers must keep accepting every location the FAT32 specification
    /// allows: any sector of the reserved area, and a backup of 0 for "none".
    #[test]
    fn validation_accepts_legitimate_fsinfo_and_backup_locations() {
        // (reserved sectors, FSInfo sector, backup boot sector)
        let cases: [(u16, u16, u16); 6] = [
            (32, 1, 6),
            (32, 1, 0),
            (32, 2, 6),
            (32, 1, 12),
            (64, 1, 12),
            (4096, 1, 6),
        ];

        for (reserved, fs_info, backup) in cases {
            let base = valid_boot_sector_with(512, 8, reserved, 2, 2_000_000, 7813);
            let mut data = base.as_bytes().to_vec();
            data[48..50].copy_from_slice(&fs_info.to_le_bytes());
            data[50..52].copy_from_slice(&backup.to_le_bytes());
            let boot = BootSector::from_bytes(&data).unwrap();
            assert_eq!(boot.fs_info_sector(), fs_info);
            assert_eq!(boot.backup_boot_sector(), backup);

            let label = format!("reserved={reserved} fsinfo={fs_info} backup={backup}");
            assert!(
                validate_boot_sector(&boot).is_ok(),
                "validation rejected a legitimate layout: {label} -> {:?}",
                validate_boot_sector(&boot)
            );
            assert!(
                validate_boot_sector_for_recovery(&boot).is_ok(),
                "recovery validation rejected a legitimate layout: {label}"
            );
        }
    }

    /// [F-10]: FSInfo and the backup boot sector live in the reserved area. A
    /// pointer outside it, or a backup on top of FSInfo, names a sector the
    /// resize would overwrite with a boot sector.
    #[test]
    fn validation_rejects_fsinfo_and_backup_outside_the_reserved_area() {
        // (FSInfo sector, backup boot sector), with 32 reserved sectors
        let cases: [(u16, u16); 6] = [
            (1, 32),     // backup on the first FAT sector
            (1, 5000),   // backup in the data area
            (1, 1),      // backup on top of FSInfo
            (0, 6),      // no FSInfo, which this tool always reads
            (32, 6),     // FSInfo on the first FAT sector
            (0xFFFF, 6), // FSInfo far outside the volume's start
        ];

        for (fs_info, backup) in cases {
            let base = valid_boot_sector_with(512, 8, 32, 2, 2_000_000, 7813);
            let mut data = base.as_bytes().to_vec();
            data[48..50].copy_from_slice(&fs_info.to_le_bytes());
            data[50..52].copy_from_slice(&backup.to_le_bytes());
            let boot = BootSector::from_bytes(&data).unwrap();

            let label = format!("fsinfo={fs_info} backup={backup}");
            assert!(
                matches!(
                    validate_boot_sector(&boot),
                    Err(Error::BootSectorValidation(_))
                ),
                "validation accepted {label}: {:?}",
                validate_boot_sector(&boot)
            );
            assert!(
                matches!(
                    validate_boot_sector_for_recovery(&boot),
                    Err(Error::BootSectorValidation(_))
                ),
                "recovery validation accepted {label}"
            );
        }
    }

    /// Safety net for [F-R52]. `validation_accepts_legitimate_geometries`
    /// pins the four supported sector sizes on the *accept* side only, so a
    /// widened set -- the exact way a literal-to-constant substitution can go
    /// wrong -- would pass unnoticed. This pins the reject side.
    #[test]
    fn an_unsupported_sector_size_is_rejected() {
        for bps in [0u16, 128, 256, 511, 513, 1000, 8192] {
            let boot = valid_boot_sector_with(bps, 1, 32, 2, 1_000_000, 7813);
            assert!(
                matches!(
                    validate_boot_sector(&boot),
                    Err(Error::BootSectorValidation(_))
                ),
                "bytes_per_sector={bps} was not rejected: {:?}",
                validate_boot_sector(&boot)
            );
        }
    }

    /// Safety net for [F-19]. The FAT type is decided by cluster count, never
    /// by the label at offset 82: formatters write "FAT32   ", "FAT     ",
    /// blanks or nothing at all there. Whatever the empty `if` around that
    /// label turns into, a valid volume must stay accepted under any label.
    #[test]
    fn validation_ignores_the_fs_type_label() {
        for label in [
            b"FAT32   ",
            b"FAT     ",
            b"FAT16   ",
            b"        ",
            b"NO NAME ",
            b"\0\0\0\0\0\0\0\0",
        ] {
            let mut raw = valid_boot_sector_with(512, 1, 32, 2, 1_000_000, 7813)
                .as_bytes()
                .to_vec();
            raw[82..90].copy_from_slice(label);
            let boot = BootSector::from_bytes(&raw).unwrap();
            assert!(
                validate_boot_sector(&boot).is_ok(),
                "label {:?} was rejected: {:?}",
                String::from_utf8_lossy(label),
                validate_boot_sector(&boot)
            );
        }
    }

    /// Safety net for [F-R52]. The 65525-cluster floor separating FAT32 from
    /// FAT16 has no test of its own: `test_not_fat32` reaches
    /// `Error::InvalidFAT32` through the root-entry-count check instead, and
    /// would still pass if the floor were deleted outright.
    #[test]
    fn a_cluster_count_below_the_fat32_floor_is_rejected() {
        // 512-byte sectors, one sector per cluster, 9,768 data sectors -- a
        // legitimate layout in every respect except that it is FAT16-sized.
        let boot = valid_boot_sector_with(512, 1, 32, 2, 10_000, 100);
        assert_eq!(
            boot.data_clusters(),
            Some(9_768),
            "fixture drifted; the case no longer sits below the floor"
        );
        assert!(
            matches!(validate_boot_sector(&boot), Err(Error::InvalidFAT32(_))),
            "a sub-65525-cluster volume was accepted as FAT32: {:?}",
            validate_boot_sector(&boot)
        );
    }

    /// [F-T02]: cluster numbers above 0x0FFFFFF6 collide with the BAD and
    /// end-of-chain markers, so a volume with more than 0x0FFFFFF5 clusters is
    /// not a usable FAT32 volume. Exactly at the ceiling is still accepted.
    #[test]
    fn a_cluster_count_above_the_fat32_ceiling_is_rejected() {
        let with_clusters = |clusters: u32| {
            let fat_size = ((clusters as u64 + 2) * 4).div_ceil(512) as u32;
            let total = 32 + 2 * fat_size + clusters;
            let boot = valid_boot_sector_with(512, 1, 32, 2, total, fat_size);
            assert_eq!(boot.data_clusters(), Some(clusters), "fixture drifted");
            boot
        };

        let at_ceiling = with_clusters(BootSector::MAX_FAT32_CLUSTERS);
        assert!(
            validate_boot_sector(&at_ceiling).is_ok(),
            "a volume at the ceiling was rejected: {:?}",
            validate_boot_sector(&at_ceiling)
        );

        let above = with_clusters(BootSector::MAX_FAT32_CLUSTERS + 1);
        assert!(
            matches!(validate_boot_sector(&above), Err(Error::InvalidFAT32(_))),
            "a volume above the ceiling was accepted as FAT32: {:?}",
            validate_boot_sector(&above)
        );
    }

    /// [F-R7]: a crafted image whose FATs are larger than the whole volume must
    /// be rejected, and rejected for the right reason. Before the fix the
    /// subtraction wrapped and the volume was reported as healthy in release
    /// builds (4291285132 data clusters) while panicking in debug builds.
    #[test]
    fn validation_rejects_a_layout_larger_than_the_volume() {
        let boot = valid_boot_sector_with(512, 8, 32, 2, 2_000_000, 15_728_640);
        let result = validate_boot_sector(&boot);

        let err = result.expect_err("a volume smaller than its own FATs must be rejected");
        let message = format!("{err}");
        assert!(
            matches!(err, Error::BootSectorValidation(_)),
            "expected a boot-sector layout error, got: {err:?}"
        );
        // The pre-fix failure mode was this being reported as a cluster-count
        // problem, which sends the user looking in entirely the wrong place.
        assert!(
            !message.contains("FAT12/16"),
            "an impossible layout must not be reported as a cluster-count problem: {message}"
        );
    }

    /// The same must hold for the recovery path: a crash-recovery run reads a
    /// boot sector with a deliberately invalidated signature, which must not
    /// become a way to skip the layout check.
    #[test]
    fn recovery_validation_also_rejects_a_layout_larger_than_the_volume() {
        let boot = valid_boot_sector_with(512, 8, 32, 2, 2_000_000, 15_728_640);
        let mut data = boot.as_bytes().to_vec();
        data[510] = 0x00;
        data[511] = 0x00;
        let invalidated = BootSector::from_bytes(&data).unwrap();

        assert!(
            validate_boot_sector_for_recovery(&invalidated).is_err(),
            "recovery validation must still reject an impossible layout"
        );
    }

    /// A total_sectors of zero is already rejected, but the layout check must
    /// not start reporting it as a layout problem - the existing, clearer error
    /// should still win.
    #[test]
    fn zero_total_sectors_is_still_reported_as_such() {
        let boot = valid_boot_sector_with(512, 8, 32, 2, 0, 7813);
        let err = validate_boot_sector(&boot).expect_err("zero total sectors must be rejected");
        assert!(
            format!("{err}").contains("Total sectors is 0"),
            "expected the total-sectors error, got: {err}"
        );
    }

    /// The recovery variant must accept exactly what the strict variant accepts,
    /// plus a zeroed signature. Both halves matter: [F-R5] hinges on the two
    /// readers disagreeing only about the signature.
    #[test]
    fn recovery_validation_differs_from_strict_only_on_the_signature() {
        let boot = valid_boot_sector_with(512, 8, 32, 2, 2_000_000, 7813);
        assert!(validate_boot_sector(&boot).is_ok());
        assert!(validate_boot_sector_for_recovery(&boot).is_ok());

        let mut data = boot.as_bytes().to_vec();
        data[510] = 0x00;
        data[511] = 0x00;
        let invalidated = BootSector::from_bytes(&data).unwrap();
        assert!(
            validate_boot_sector(&invalidated).is_err(),
            "strict validation must reject a zeroed signature"
        );
        assert!(
            validate_boot_sector_for_recovery(&invalidated).is_ok(),
            "recovery validation must accept a zeroed signature"
        );
    }

    /// [F-R5]: an interrupted resize and a corrupt boot sector must be
    /// distinguishable by *type*, not by the text of a formatted message. The
    /// CLI's recovery decision keys on this variant; if strict validation ever
    /// folds the zeroed signature back into `BootSectorValidation`, recovery
    /// silently stops being offered.
    #[test]
    fn an_invalidated_signature_is_a_distinct_error_variant() {
        let boot = valid_boot_sector_with(512, 8, 32, 2, 2_000_000, 7813);

        let mut data = boot.as_bytes().to_vec();
        data[510] = 0x00;
        data[511] = 0x00;
        let invalidated = BootSector::from_bytes(&data).unwrap();

        assert!(
            matches!(
                validate_boot_sector(&invalidated),
                Err(Error::BootSectorInvalidated)
            ),
            "a zeroed signature must be its own variant, not a formatted string"
        );

        // A merely-wrong signature must NOT get the recovery-bearing variant.
        let mut data = boot.as_bytes().to_vec();
        data[510] = 0x34;
        data[511] = 0x12;
        let corrupt = BootSector::from_bytes(&data).unwrap();

        assert!(
            matches!(
                validate_boot_sector(&corrupt),
                Err(Error::BootSectorValidation(_))
            ),
            "a corrupt signature must stay a plain validation failure"
        );
    }

    /// The recovery reader's extra permissiveness must extend to the zeroed
    /// signature and nothing else. A signature that is merely *wrong* is a
    /// corrupt or foreign device, and [F-R5]'s fix must not widen the
    /// invalidated case into "any bad signature" while making it a distinct
    /// error variant.
    #[test]
    fn a_garbage_signature_is_rejected_by_both_readers() {
        let boot = valid_boot_sector_with(512, 8, 32, 2, 2_000_000, 7813);

        for sig in [0x1234u16, 0xFFFF, 0x00AA, 0xAA00, 0x55AA] {
            let mut data = boot.as_bytes().to_vec();
            data[510] = (sig & 0xFF) as u8;
            data[511] = (sig >> 8) as u8;
            let corrupt = BootSector::from_bytes(&data).unwrap();

            assert!(
                validate_boot_sector(&corrupt).is_err(),
                "strict validation must reject signature {sig:#06X}"
            );
            assert!(
                validate_boot_sector_for_recovery(&corrupt).is_err(),
                "recovery validation must reject signature {sig:#06X} -- only a \
                 zeroed signature marks an interrupted resize"
            );
        }
    }

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
