//! The resize algorithm.
//!
//! [F-R42] moved the checkpoint format, the option/result DTOs, the
//! read-only filesystem query and the fault-injection hook out of this file.
//! What remains is the operation itself.

use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::{
    boot_sectors_match, open_fat32_device, read_backup_boot_sector, read_boot_sector_for_recovery,
    read_fat_table, read_fsinfo, write_backup_boot_sector, write_boot_sector, write_fsinfo,
    BootSector, FSInfo,
};
use crate::resize::calculator::{affected_cluster_range, calculate_new_size, SizeCalculation};
use crate::resize::checkpoint::{
    check_for_incomplete_resize, clear_checkpoint, write_checkpoint, ResizeCheckpoint, ResizePhase,
    PROGRESS_RECORD_EVERY,
};
use crate::resize::fault::maybe_crash_at;
use crate::resize::options::{ResizeOptions, ResizeResult};
use crate::resize::relocator::{execute_relocation, plan_relocation, ShiftProgress};
use crate::system::check_not_mounted;

/// Main resize function with crash-safe checkpoint support
pub fn resize_fat32(options: ResizeOptions) -> Result<ResizeResult> {
    let mut operations = Vec::new();

    // Check if mounted
    check_not_mounted(options.device_path())?;
    operations.push("Verified device is not mounted".to_string());

    // Open device. The sector size is established here, before any
    // sector-addressed I/O is possible ([F-R23]).
    let device = open_fat32_device(options.device_path(), !options.is_dry_run())?;
    operations.push(format!(
        "Opened device: {}",
        options.device_path().display()
    ));

    // Read boot sector - use recovery mode to allow invalidated signature
    // from an interrupted resize operation
    let mut boot = read_boot_sector_for_recovery(&device)?;
    operations.push(format!(
        "Read boot sector ({}-byte sectors)",
        boot.bytes_per_sector()
    ));

    // Check for incomplete resize operation
    let incomplete_resize = if !options.is_dry_run() {
        check_for_incomplete_resize(&device, &boot)?
    } else {
        None
    };

    if let Some(ref checkpoint) = incomplete_resize {
        eprintln!(
            "Resuming interrupted resize from phase {:?}...",
            checkpoint.phase
        );
        operations.push(format!(
            "Detected incomplete resize at phase {:?}",
            checkpoint.phase
        ));

        // Validate that device size matches checkpoint
        let device_sectors = device.total_sectors();
        if device_sectors < checkpoint.new_total_sectors as u64 {
            return Err(Error::ResizeSizeMismatch(checkpoint.phase as u8));
        }
    }

    // Read backup boot sector (skip match check if boot sector is invalidated)
    let backup_sector = boot.backup_boot_sector();
    let backup_boot = read_backup_boot_sector(&device, backup_sector)?;
    if boot.is_signature_valid() && !boot_sectors_match(&boot, &backup_boot) {
        return Err(Error::BackupMismatch);
    }
    operations.push(format!(
        "Verified backup boot sector at sector {}",
        backup_sector
    ));

    // Read FSInfo
    let fsinfo_sector = boot.fs_info_sector();
    let mut fsinfo = read_fsinfo(&device, fsinfo_sector)?;
    operations.push(format!("Read FSInfo from sector {}", fsinfo_sector));

    // Calculate new size (use checkpoint values if resuming)
    let device_sectors = device.total_sectors();
    let calculation = if let Some(ref checkpoint) = incomplete_resize {
        // Use checkpoint values for consistency
        let (first_affected_cluster, last_affected_cluster) =
            affected_cluster_range(&boot, checkpoint.old_fat_size, checkpoint.new_fat_size);

        // A checkpoint carries the geometry a previous run committed to. If that
        // geometry does not describe a filesystem that fits, the record is not
        // one we can resume from.
        let new_data_clusters = boot
            .with_geometry(checkpoint.new_total_sectors, checkpoint.new_fat_size)
            .data_clusters()
            .ok_or_else(|| {
                Error::Calculation(format!(
                    "Checkpoint geometry does not fit: {} sectors with a FAT of {} sectors",
                    checkpoint.new_total_sectors, checkpoint.new_fat_size
                ))
            })?;

        SizeCalculation {
            old_total_sectors: checkpoint.old_total_sectors,
            new_total_sectors: checkpoint.new_total_sectors,
            old_fat_size: checkpoint.old_fat_size,
            new_fat_size: checkpoint.new_fat_size,
            new_data_clusters,
            new_free_clusters: 0, // Will be recalculated
            fat_needs_growth: checkpoint.new_fat_size > checkpoint.old_fat_size,
            fat_growth_sectors: checkpoint
                .new_fat_size
                .saturating_sub(checkpoint.old_fat_size),
            first_affected_cluster,
            last_affected_cluster,
        }
    } else {
        calculate_new_size(&boot, device_sectors)?
    };

    operations.push(format!(
        "Calculated resize: {} -> {} sectors",
        calculation.old_total_sectors, calculation.new_total_sectors
    ));

    if options.is_verbose() {
        print_verbose_resize_info(&boot, &calculation);
    }

    let old_size_bytes = calculation.old_total_sectors as u64 * boot.bytes_per_sector() as u64;
    let new_size_bytes = calculation.new_total_sectors as u64 * boot.bytes_per_sector() as u64;

    // Read FAT table
    let fat = read_fat_table(&device, &boot, 0)?;
    operations.push(format!("Read FAT table ({} entries)", fat.len()));

    let mut clusters_relocated = 0;

    // Determine starting phase based on checkpoint
    let starting_phase = incomplete_resize
        .as_ref()
        .map(|cp| cp.phase)
        .unwrap_or(ResizePhase::Started);

    // Handle FAT growth if needed
    if calculation.fat_needs_growth {
        operations.push(format!(
            "FAT needs to grow by {} sectors",
            calculation.fat_growth_sectors
        ));

        // The boot sector as it will be committed once the resize completes.
        // The relocator reads the new data-area start straight off it, so the
        // plan cannot disagree with the geometry the filesystem ends up with.
        let new_boot = boot.with_geometry(calculation.new_total_sectors, calculation.new_fat_size);

        // Plan cluster data shift
        let plan = plan_relocation(&boot, &new_boot, &fat, calculation.first_affected_cluster)?;

        if !plan.is_empty() {
            operations.push(format!(
                "Planned data shift for {} clusters ({} bytes)",
                plan.cluster_count(),
                plan.total_bytes
            ));

            if options.is_verbose() {
                eprintln!("\nData shift plan (cluster numbers unchanged, sectors shift forward):");
                eprintln!("  {} clusters will be moved", plan.moves.len());
            }

            if !options.is_dry_run() {
                // === PHASE 0: Data shift (safe - source preserved) ===
                if starting_phase == ResizePhase::Started {
                    // How far a previous pass got. A fresh run has moved
                    // nothing, so it resumes from the highest cluster in the
                    // plan; a resume must read the recorded position, because
                    // restarting the shift from the top would re-read source
                    // sectors the previous pass overwrote.
                    let resume_from = match incomplete_resize.as_ref() {
                        None => plan.highest_cluster(),
                        Some(cp) => cp.shift_progress.ok_or_else(|| {
                            Error::Relocation(
                                "interrupted data shift left no progress record, so the point \
                                 it reached is unknown; resuming would copy already-shifted \
                                 data over itself. The filesystem is intact but must be \
                                 recovered from backup or repaired by hand."
                                    .to_string(),
                            )
                        })?,
                    };

                    // Redoing a move is only safe while its source is intact,
                    // and `old_home(c)` survives until the pass descends
                    // `shift_clusters` below it. Recording at least that often
                    // keeps every replayed move readable.
                    //
                    // A shift shorter than one cluster overlaps a cluster with
                    // itself, so no move may ever be replayed. A single pass is
                    // still safe - each cluster is read whole before it is
                    // written - so the run proceeds, but nothing is recorded:
                    // a later resume then finds no progress record and refuses
                    // rather than corrupting.
                    let shift_clusters = plan.shift_clusters(&boot);
                    let resumable = shift_clusters > 0;
                    let record_every = if resumable {
                        PROGRESS_RECORD_EVERY.min(shift_clusters)
                    } else {
                        u32::MAX
                    };

                    // Write initial checkpoint
                    let mut checkpoint = ResizeCheckpoint::new(
                        ResizePhase::Started,
                        calculation.old_total_sectors,
                        calculation.new_total_sectors,
                        calculation.old_fat_size,
                        calculation.new_fat_size,
                    );
                    if resumable {
                        checkpoint.shift_progress = Some(resume_from);
                    }
                    write_checkpoint(&device, &checkpoint)?;
                    operations.push("Wrote checkpoint (phase 0: started)".to_string());

                    maybe_crash_at("after_checkpoint_start");

                    // Execute data shift, recording how far it has got as it
                    // descends so a crash here can be resumed rather than
                    // replayed from the beginning.
                    let mut record = |next_cluster: u32| -> Result<()> {
                        if !resumable {
                            return Ok(());
                        }
                        let mut cp = checkpoint.clone();
                        cp.shift_progress = Some(next_cluster);
                        write_checkpoint(&device, &cp)
                    };
                    let mut progress = ShiftProgress {
                        resume_from,
                        record_every,
                        record: &mut record,
                    };
                    execute_relocation(&device, &boot, &plan, &mut progress, options.is_verbose())?;
                    clusters_relocated = plan.cluster_count();
                    operations.push(format!("Shifted {} clusters forward", clusters_relocated));

                    maybe_crash_at("after_data_shift");

                    // Update checkpoint to phase 1
                    let checkpoint = ResizeCheckpoint::new(
                        ResizePhase::DataCopied,
                        calculation.old_total_sectors,
                        calculation.new_total_sectors,
                        calculation.old_fat_size,
                        calculation.new_fat_size,
                    );
                    write_checkpoint(&device, &checkpoint)?;
                    operations.push("Updated checkpoint (phase 1: data copied)".to_string());

                    maybe_crash_at("after_checkpoint_data_copied");
                } else {
                    operations.push("Skipping data shift (already done)".to_string());
                    clusters_relocated = plan.cluster_count();
                }

                // === PHASE 1: FAT operations (dangerous - boot sector invalidated) ===
                if starting_phase <= ResizePhase::DataCopied {
                    // === DANGER ZONE START ===
                    // Invalidate boot sector to prevent other tools from operating
                    boot.invalidate_signature();
                    write_boot_sector(&device, &boot)?;
                    device.sync()?;
                    operations.push("Invalidated boot sector (danger zone)".to_string());

                    maybe_crash_at("after_boot_invalidate");

                    // Initialize new FAT1 sectors
                    init_new_fat_sectors(&device, &boot, &calculation)?;
                    operations.push("Initialized new FAT sectors".to_string());

                    // Sync FAT1 to FAT2
                    sync_fat_copies(&device, &boot, &calculation)?;
                    operations.push("Synced FAT copies".to_string());

                    maybe_crash_at("after_fat_write");

                    // Update checkpoint to phase 2
                    let checkpoint = ResizeCheckpoint::new(
                        ResizePhase::FatWritten,
                        calculation.old_total_sectors,
                        calculation.new_total_sectors,
                        calculation.old_fat_size,
                        calculation.new_fat_size,
                    );
                    write_checkpoint(&device, &checkpoint)?;
                    operations.push("Updated checkpoint (phase 2: FAT written)".to_string());

                    maybe_crash_at("after_checkpoint_fat_written");
                    // === DANGER ZONE END ===
                } else {
                    operations.push("Skipping FAT operations (already done)".to_string());
                }
            } else {
                operations.push("Dry run: would shift cluster data".to_string());
            }
        }
    }

    if !options.is_dry_run() {
        // === PHASE 2: Metadata update (restore boot sector) ===

        // Update boot sector with new values and restore signature
        boot = boot.with_geometry(calculation.new_total_sectors, calculation.new_fat_size);
        boot.restore_signature(); // Restore 0xAA55 signature

        write_boot_sector(&device, &boot)?;
        operations.push("Updated boot sector (signature restored)".to_string());

        // Update backup boot sector
        write_backup_boot_sector(&device, &boot, backup_sector)?;
        operations.push("Updated backup boot sector".to_string());

        // Update FSInfo with new free cluster count
        let old_free = fsinfo.free_count();
        // The old geometry came off a filesystem that validated, so it fits;
        // falling back to the new count yields a zero delta, which leaves the
        // free-cluster hint untouched rather than inventing free space.
        let old_data_clusters = boot
            .with_geometry(calculation.old_total_sectors, calculation.old_fat_size)
            .data_clusters()
            .unwrap_or(calculation.new_data_clusters);
        let additional_clusters = calculation
            .new_data_clusters
            .saturating_sub(old_data_clusters);

        let new_free = if old_free == FSInfo::UNKNOWN_FREE {
            FSInfo::UNKNOWN_FREE
        } else {
            old_free.saturating_add(additional_clusters)
        };
        fsinfo.set_free_count(new_free);
        write_fsinfo(&device, &fsinfo, fsinfo_sector)?;
        operations.push(format!("Updated FSInfo (free clusters: {})", new_free));

        // Clear checkpoint
        clear_checkpoint(&device)?;
        operations.push("Cleared checkpoint".to_string());

        // Final sync
        device.sync()?;
        operations.push("Synced changes to disk".to_string());
    } else {
        operations.push("Dry run: no changes made".to_string());
    }

    Ok(ResizeResult {
        old_size_bytes,
        new_size_bytes,
        fat_grew: calculation.fat_needs_growth,
        clusters_relocated,
        calculation,
        operations,
    })
}

/// Print verbose resize information to stderr
fn print_verbose_resize_info(boot: &BootSector, calculation: &SizeCalculation) {
    eprintln!("Current filesystem:");
    eprintln!("  Total sectors: {}", calculation.old_total_sectors);
    eprintln!("  FAT size: {} sectors", calculation.old_fat_size);
    match boot.data_clusters() {
        Some(clusters) => eprintln!("  Data clusters: {}", clusters),
        None => eprintln!("  Data clusters: <layout does not fit in the volume>"),
    }
    eprintln!();
    eprintln!("After resize:");
    eprintln!("  Total sectors: {}", calculation.new_total_sectors);
    eprintln!("  FAT size: {} sectors", calculation.new_fat_size);
    eprintln!("  Data clusters: {}", calculation.new_data_clusters);
    eprintln!("  FAT needs growth: {}", calculation.fat_needs_growth);
}

/// Initialize new FAT sectors with free entries (zeros)
/// This must be called BEFORE relocation so that reading new FAT sectors returns valid data
fn init_new_fat_sectors(device: &Device, boot: &BootSector, calc: &SizeCalculation) -> Result<()> {
    let bytes_per_sector = boot.bytes_per_sector() as usize;

    if calc.new_fat_size <= calc.old_fat_size {
        return Ok(()); // No extension needed
    }

    let fat1_start = boot.first_fat_sector();

    // Create a sector of free entries
    let free_sector = vec![0u8; bytes_per_sector];

    // Initialize new sectors of FAT1 with free entries
    for sector_offset in calc.old_fat_size..calc.new_fat_size {
        let sector = fat1_start + sector_offset as u64;
        device.write_sector(sector, &free_sector)?;
    }

    Ok(())
}

/// Sync FAT1 to FAT2 (and any additional FAT copies)
/// This must be called AFTER relocation so that FAT2 gets the updated entries
fn sync_fat_copies(device: &Device, boot: &BootSector, calc: &SizeCalculation) -> Result<()> {
    if calc.new_fat_size <= calc.old_fat_size {
        return Ok(()); // No extension needed
    }

    let fat1_start = boot.first_fat_sector();

    // Copy the entire FAT1 to FAT2 (and any additional FAT copies)
    // FAT2 starts right after FAT1's NEW size
    for fat_num in 1..boot.num_fats() {
        let fat_dest_start = fat1_start + (fat_num as u64 * calc.new_fat_size as u64);

        // Copy all sectors from FAT1 to this FAT copy
        for sector_offset in 0..calc.new_fat_size {
            let src_sector = fat1_start + sector_offset as u64;
            let dest_sector = fat_dest_start + sector_offset as u64;

            let data = device.read_sector(src_sector)?;
            device.write_sector(dest_sector, &data)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::resize::testutil::test_boot_sector;

    /// `with_geometry(..).data_clusters()` is now the single data-cluster path,
    /// and must produce the layout the FAT32 spec calls for. The FSInfo free
    /// count and the resize plan are both derived from it.
    ///
    /// Expected values are computed here independently of the accessor:
    /// `(total_sectors - reserved - num_fats * fat_size) / sectors_per_cluster`,
    /// with 32 reserved sectors, 2 FATs and 8 sectors per cluster.
    #[test]
    fn test_data_clusters_for_a_projected_geometry() {
        let base = test_boot_sector(1_000_000, 1000);

        for (total_sectors, fat_size) in [(1_000_000u32, 1000u32), (4_000_000, 3903), (65_536, 64)]
        {
            let expected = (total_sectors - 32 - 2 * fat_size) / 8;

            assert_eq!(
                base.with_geometry(total_sectors, fat_size).data_clusters(),
                Some(expected),
                "wrong cluster count at total_sectors={} fat_size={}",
                total_sectors,
                fat_size
            );
        }
    }

    /// `with_geometry` must change only the two geometry fields, leaving the
    /// rest of the sector - signature included - byte-for-byte intact.
    #[test]
    fn test_with_geometry_preserves_the_rest_of_the_sector() {
        let base = test_boot_sector(1_000_000, 1000);
        let projected = base.with_geometry(4_000_000, 3903);

        assert_eq!(projected.total_sectors_32(), 4_000_000);
        assert_eq!(projected.fat_size_32(), 3903);
        assert_eq!(projected.reserved_sectors(), base.reserved_sectors());
        assert_eq!(projected.num_fats(), base.num_fats());
        assert_eq!(projected.sectors_per_cluster(), base.sectors_per_cluster());
        assert_eq!(projected.root_cluster(), base.root_cluster());
        assert!(projected.is_signature_valid());

        let mut expected = base.as_bytes().to_vec();
        expected[32..36].copy_from_slice(&4_000_000u32.to_le_bytes());
        expected[36..40].copy_from_slice(&3903u32.to_le_bytes());
        assert_eq!(projected.as_bytes(), expected.as_slice());
    }
}
