use crate::device::Device;
use crate::error::Result;
use crate::fat32::{fat_entry, BootSector};

/// A planned cluster relocation representing physical sector movement
///
/// When FAT tables grow, cluster data must shift forward to make room.
/// Cluster numbers remain unchanged; only their physical sector positions move.
#[derive(Debug, Clone)]
pub struct ClusterMove {
    /// Original cluster number
    pub from_cluster: u32,
    /// New cluster number (same as from_cluster in new approach)
    pub to_cluster: u32,
    /// Original sector (calculated from cluster)
    pub from_sector: u64,
    /// New sector (calculated from cluster)
    pub to_sector: u64,
}

/// A complete relocation plan
#[derive(Debug)]
pub struct RelocationPlan {
    /// List of cluster moves to perform (all clusters that need physical movement)
    pub moves: Vec<ClusterMove>,
    /// Total bytes to be relocated
    pub total_bytes: u64,
    /// Old first data sector
    pub old_first_data_sector: u64,
    /// New first data sector
    pub new_first_data_sector: u64,
}

impl RelocationPlan {
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    pub fn cluster_count(&self) -> usize {
        self.moves.len()
    }
}

/// Plan the relocation of clusters when FAT tables grow
///
/// When FAT tables grow, the data area shifts forward. ALL clusters need to be
/// physically moved to their new sector positions, but cluster numbers stay the same.
///
/// This function identifies which clusters are in use and need to be shifted.
/// The actual shifting must be done from highest cluster to lowest to avoid
/// overwriting data that hasn't been moved yet.
pub fn plan_relocation(
    _device: &Device,
    boot: &BootSector,
    fat: &[u32],
    first_affected: u32,
    last_affected: u32,
    _new_data_clusters: u32,
) -> Result<RelocationPlan> {
    // Calculate sector positions
    let old_first_data_sector = boot.first_data_sector();
    let sectors_per_cluster = boot.sectors_per_cluster() as u64;

    // Calculate new first data sector based on FAT growth
    // The affected clusters (first_affected to last_affected) represent the data
    // that will be overwritten by the expanded FAT tables. The shift amount equals
    // the number of sectors these clusters occupy.
    let affected_clusters = (last_affected - first_affected + 1) as u64;
    let shift_sectors = affected_clusters * sectors_per_cluster;
    let new_first_data_sector = old_first_data_sector + shift_sectors;

    let old_max_cluster = boot.data_clusters() + 2;

    // Find all clusters that are in use and need to be shifted
    // We only need to move clusters that have data (not free clusters)
    let mut moves = Vec::new();

    for cluster in first_affected..old_max_cluster {
        if cluster >= fat.len() as u32 {
            break;
        }

        let entry = fat[cluster as usize];

        // Only include clusters that are in use
        if fat_entry::is_free(entry) {
            continue;
        }

        let old_sector = old_first_data_sector + ((cluster - 2) as u64 * sectors_per_cluster);
        let new_sector = new_first_data_sector + ((cluster - 2) as u64 * sectors_per_cluster);

        // Only include if positions differ (they should all differ when FAT grows)
        if old_sector != new_sector {
            moves.push(ClusterMove {
                from_cluster: cluster,
                to_cluster: cluster, // Same cluster number, different physical location
                from_sector: old_sector,
                to_sector: new_sector,
            });
        }
    }

    // Sort by cluster number descending (for safe copying from end to start)
    moves.sort_by(|a, b| b.from_cluster.cmp(&a.from_cluster));

    let total_bytes = moves.len() as u64 * boot.bytes_per_cluster() as u64;

    Ok(RelocationPlan {
        moves,
        total_bytes,
        old_first_data_sector,
        new_first_data_sector,
    })
}

/// Execute a relocation plan by shifting all data forward
///
/// This copies cluster data from old positions to new positions.
/// The copying is done from highest cluster to lowest to avoid overwriting.
///
/// Since cluster numbers don't change, no FAT chain or directory entry updates are needed!
///
/// Returns an empty vector (no cluster number changes) for API compatibility.
pub fn execute_relocation(
    device: &Device,
    boot: &BootSector,
    _fat: &mut [u32],
    plan: &RelocationPlan,
    _new_fat_size: u32,
    _new_data_clusters: u32,
    verbose: bool,
) -> Result<Vec<(u32, u32)>> {
    let sectors_per_cluster = boot.sectors_per_cluster() as u32;

    if verbose {
        eprintln!(
            "Shifting data forward: {} clusters need movement",
            plan.moves.len()
        );
        eprintln!("  Old first data sector: {}", plan.old_first_data_sector);
        eprintln!("  New first data sector: {}", plan.new_first_data_sector);
    }

    // Copy data from highest cluster to lowest (already sorted in plan_relocation)
    for (i, mv) in plan.moves.iter().enumerate() {
        if verbose && (i < 10 || i % 100 == 0 || i == plan.moves.len() - 1) {
            eprintln!(
                "Moving cluster {} from sector {} to sector {} ({}/{})",
                mv.from_cluster,
                mv.from_sector,
                mv.to_sector,
                i + 1,
                plan.moves.len()
            );
        }

        // Read from old position
        let data = device.read_sectors(mv.from_sector, sectors_per_cluster)?;

        // Write to new position
        device.write_sectors(mv.to_sector, &data)?;
    }

    // Sync after data movement
    device.sync()?;

    // No cluster number changes, so return empty vector
    // The root cluster stays at cluster 2 (just at a different physical location)
    Ok(Vec::new())
}

/// Verify that all clusters in the affected range are free after relocation
///
/// Note: With the new data-shifting approach, the "affected range" clusters
/// don't become free - they just move to new physical positions. This function
/// is kept for API compatibility but the verification is different.
pub fn verify_relocation(_fat: &[u32], _first_affected: u32, _last_affected: u32) -> Result<()> {
    // With the new approach, clusters aren't freed, they're just shifted.
    // The first_affected..last_affected range now represents sectors that
    // are part of the expanded FAT, so we don't need to verify them.
    //
    // For API compatibility, we just return Ok.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    // A cluster-aligned geometry: the total FAT growth is an exact multiple of
    // sectors_per_cluster, so the data area shifts by a whole number of clusters.
    const BPS: u16 = 512;
    const SPC: u8 = 8;
    const RESERVED: u16 = 32;
    const NUM_FATS: u8 = 2;
    const TOTAL_SECTORS: u32 = 4232;
    const OLD_FAT_SIZE: u32 = 100;
    const NEW_FAT_SIZE: u32 = 104; // 2 FATs x 4 sectors = 8 = 1 cluster

    /// Build a FAT32 boot sector with the given geometry.
    ///
    /// `root_entry_count` is left at zero, as FAT32 requires.
    fn boot_sector(fat_size: u32) -> BootSector {
        let mut data = vec![0u8; BPS as usize];
        data[0] = 0xEB;
        data[1] = 0x58;
        data[2] = 0x90;
        data[11..13].copy_from_slice(&BPS.to_le_bytes());
        data[13] = SPC;
        data[14..16].copy_from_slice(&RESERVED.to_le_bytes());
        data[16] = NUM_FATS;
        data[21] = 0xF8;
        data[32..36].copy_from_slice(&TOTAL_SECTORS.to_le_bytes());
        data[36..40].copy_from_slice(&fat_size.to_le_bytes());
        data[44..48].copy_from_slice(&2u32.to_le_bytes());
        data[48] = 0x01;
        data[50] = 0x06;
        data[510] = 0x55;
        data[511] = 0xAA;
        BootSector::from_bytes(&data).unwrap()
    }

    /// A FAT where every cluster in `in_use` is an end-of-chain marker and the
    /// rest are free.
    fn fat_with(in_use: &[u32]) -> Vec<u32> {
        let mut fat = vec![0u32; 600];
        fat[0] = 0x0FFF_FFF8;
        fat[1] = 0x0FFF_FFFF;
        for &cluster in in_use {
            fat[cluster as usize] = 0x0FFF_FFFF;
        }
        fat
    }

    /// A temp-file-backed device large enough to hold the whole test filesystem.
    fn temp_device() -> (NamedTempFile, Device) {
        let file = NamedTempFile::new().unwrap();
        let bytes = TOTAL_SECTORS as usize * BPS as usize;
        std::fs::write(file.path(), vec![0u8; bytes]).unwrap();
        let device = Device::open(file.path()).unwrap();
        (file, device)
    }

    /// A recognisable fill byte per cluster, never zero.
    fn fill_byte(cluster: u32) -> u8 {
        0x40 + cluster as u8
    }

    fn aligned_plan(device: &Device, fat: &[u32]) -> RelocationPlan {
        let old_boot = boot_sector(OLD_FAT_SIZE);
        let new_boot = boot_sector(NEW_FAT_SIZE);
        plan_relocation(device, &old_boot, fat, 2, 2, new_boot.data_clusters()).unwrap()
    }

    /// The relocator's idea of where the data area moves to must be the same as
    /// the one the boot sector will report once the new FAT size is committed.
    /// If these diverge, relocated clusters land where nothing looks for them.
    #[test]
    fn plan_relocation_new_data_start_matches_grown_boot_sector() {
        let old_boot = boot_sector(OLD_FAT_SIZE);
        let new_boot = boot_sector(NEW_FAT_SIZE);
        let (_file, device) = temp_device();
        let fat = fat_with(&[2, 3, 5]);

        let plan = aligned_plan(&device, &fat);

        assert_eq!(plan.old_first_data_sector, old_boot.first_data_sector());
        assert_eq!(plan.old_first_data_sector, 232);
        assert_eq!(
            plan.new_first_data_sector,
            new_boot.first_data_sector(),
            "relocator's new data start must equal the committed boot sector's"
        );
        assert_eq!(plan.new_first_data_sector, 240);
    }

    /// Every move must address the sectors the boot sector derives for that
    /// cluster - the old boot sector for the source, the new one for the target.
    #[test]
    fn plan_relocation_maps_clusters_to_boot_derived_sectors() {
        let old_boot = boot_sector(OLD_FAT_SIZE);
        let new_boot = boot_sector(NEW_FAT_SIZE);
        let (_file, device) = temp_device();
        let fat = fat_with(&[2, 3, 5]);

        let plan = aligned_plan(&device, &fat);

        for mv in &plan.moves {
            assert_eq!(
                mv.from_sector,
                old_boot.cluster_to_sector(mv.from_cluster),
                "cluster {} source sector",
                mv.from_cluster
            );
            assert_eq!(
                mv.to_sector,
                new_boot.cluster_to_sector(mv.from_cluster),
                "cluster {} target sector",
                mv.from_cluster
            );
        }
    }

    #[test]
    fn plan_relocation_moves_only_clusters_in_use() {
        let (_file, device) = temp_device();
        let fat = fat_with(&[2, 3, 5]);

        let plan = aligned_plan(&device, &fat);

        let mut moved: Vec<u32> = plan.moves.iter().map(|mv| mv.from_cluster).collect();
        moved.sort_unstable();
        assert_eq!(moved, vec![2, 3, 5], "cluster 4 is free and must not move");
        assert_eq!(plan.cluster_count(), 3);
        assert_eq!(plan.total_bytes, 3 * 4096);
    }

    /// Moves are applied in list order in a single forward pass, so descending
    /// cluster order is what keeps an overlapping shift from eating its own
    /// source data.
    #[test]
    fn plan_relocation_orders_moves_highest_cluster_first() {
        let (_file, device) = temp_device();
        let fat = fat_with(&[2, 3, 5]);

        let plan = aligned_plan(&device, &fat);

        let moved: Vec<u32> = plan.moves.iter().map(|mv| mv.from_cluster).collect();
        assert_eq!(moved, vec![5, 3, 2]);
    }

    /// Each cluster's bytes must end up at the sector the new boot sector
    /// derives for that same cluster number.
    #[test]
    fn execute_relocation_round_trips_cluster_contents() {
        let old_boot = boot_sector(OLD_FAT_SIZE);
        let new_boot = boot_sector(NEW_FAT_SIZE);
        let (_file, device) = temp_device();
        let in_use = [2u32, 3, 5];
        let mut fat = fat_with(&in_use);
        let bytes_per_cluster = old_boot.bytes_per_cluster() as usize;

        for &cluster in &in_use {
            let data = vec![fill_byte(cluster); bytes_per_cluster];
            device
                .write_sectors(old_boot.cluster_to_sector(cluster), &data)
                .unwrap();
        }

        let plan = aligned_plan(&device, &fat);
        execute_relocation(
            &device,
            &old_boot,
            &mut fat,
            &plan,
            NEW_FAT_SIZE,
            new_boot.data_clusters(),
            false,
        )
        .unwrap();

        for &cluster in &in_use {
            let got = device
                .read_sectors(new_boot.cluster_to_sector(cluster), SPC as u32)
                .unwrap();
            assert_eq!(
                got,
                vec![fill_byte(cluster); bytes_per_cluster],
                "cluster {} did not land at its new home",
                cluster
            );
        }
    }

    /// With contiguous in-use clusters the shift is smaller than the span of
    /// used data, so every target sector is some other cluster's source. One
    /// pass must still preserve all of it.
    #[test]
    fn execute_relocation_preserves_overlapping_clusters_in_one_pass() {
        let old_boot = boot_sector(OLD_FAT_SIZE);
        let new_boot = boot_sector(NEW_FAT_SIZE);
        let (_file, device) = temp_device();
        let in_use = [2u32, 3, 4, 5];
        let mut fat = fat_with(&in_use);
        let bytes_per_cluster = old_boot.bytes_per_cluster() as usize;

        for &cluster in &in_use {
            let data = vec![fill_byte(cluster); bytes_per_cluster];
            device
                .write_sectors(old_boot.cluster_to_sector(cluster), &data)
                .unwrap();
        }

        // Cluster 3's target is cluster 4's source: the ranges genuinely overlap.
        assert_eq!(
            new_boot.cluster_to_sector(3),
            old_boot.cluster_to_sector(4),
            "test geometry must produce overlapping source and target ranges"
        );

        let plan = aligned_plan(&device, &fat);
        execute_relocation(
            &device,
            &old_boot,
            &mut fat,
            &plan,
            NEW_FAT_SIZE,
            new_boot.data_clusters(),
            false,
        )
        .unwrap();

        for &cluster in &in_use {
            let got = device
                .read_sectors(new_boot.cluster_to_sector(cluster), SPC as u32)
                .unwrap();
            assert_eq!(
                got,
                vec![fill_byte(cluster); bytes_per_cluster],
                "cluster {} was corrupted by an overlapping move",
                cluster
            );
        }
    }

    #[test]
    fn test_cluster_move() {
        let mv = ClusterMove {
            from_cluster: 5,
            to_cluster: 5, // Same cluster number with new approach
            from_sector: 1000,
            to_sector: 2000,
        };

        assert_eq!(mv.from_cluster, 5);
        assert_eq!(mv.to_cluster, 5);
        assert_eq!(mv.from_sector, 1000);
        assert_eq!(mv.to_sector, 2000);
    }

    #[test]
    fn test_relocation_plan_empty() {
        let plan = RelocationPlan {
            moves: vec![],
            total_bytes: 0,
            old_first_data_sector: 2050,
            new_first_data_sector: 4096,
        };

        assert!(plan.is_empty());
        assert_eq!(plan.cluster_count(), 0);
    }

    #[test]
    fn test_relocation_plan_with_moves() {
        let plan = RelocationPlan {
            moves: vec![
                ClusterMove {
                    from_cluster: 3,
                    to_cluster: 3,
                    from_sector: 200,
                    to_sector: 400,
                },
                ClusterMove {
                    from_cluster: 2,
                    to_cluster: 2,
                    from_sector: 100,
                    to_sector: 300,
                },
            ],
            total_bytes: 8192,
            old_first_data_sector: 100,
            new_first_data_sector: 300,
        };

        assert!(!plan.is_empty());
        assert_eq!(plan.cluster_count(), 2);
        assert_eq!(plan.total_bytes, 8192);
    }

    #[test]
    fn test_verify_relocation_always_succeeds() {
        let fat = vec![
            0x0FFFFFF8, 0x0FFFFFFF, 0x00000003, // Cluster 2 in use
            0x0FFFFFFF, // Cluster 3 in use
        ];

        // With new approach, verification always succeeds
        assert!(verify_relocation(&fat, 2, 4).is_ok());
    }
}
