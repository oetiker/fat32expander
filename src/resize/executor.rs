//! The resize algorithm.
//!
//! [F-R42] moved the checkpoint format, the option/result DTOs, the
//! read-only filesystem query and the fault-injection hook out of this file.
//! What remains is the operation itself.

use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::{
    boot_sectors_match, copy_fat_to_other_copies, fat_entry, open_fat32_device,
    read_backup_boot_sector, read_boot_sector_for_recovery, read_fat_table, read_fsinfo,
    write_backup_boot_sector, write_boot_sector, write_fsinfo, zero_fat_extension, BootSector,
    FSInfo,
};
use crate::resize::calculator::{calculate_new_size, SizeCalculation};
use crate::resize::checkpoint::{
    check_for_incomplete_resize, clear_checkpoint, mark_checkpoint_slot, write_checkpoint,
    CheckpointSlot, ResizeCheckpoint, ResizePhase,
};
use crate::resize::events::{Reporter, ResizeEvent};
use crate::resize::fault::{maybe_crash_at, CrashPoint};
use crate::resize::options::{ResizeOptions, ResizeResult};
use crate::resize::relocator::{
    execute_relocation, plan_relocation, RelocationPlan, ShiftProgress,
};
use crate::system::check_not_mounted;

// ===== Phase witnesses ([F-R15]) =====
//
// A crash-safe resize has three phases that must run in this order and at most
// once each: shift the cluster data, write the FAT extension, commit the new
// geometry. That ordering used to be enforced by two `if starting_phase ...`
// guards and a pair of `// === DANGER ZONE ===` comments inside a single
// 400-line function. Nothing stopped a later edit from reordering the blocks,
// running one twice, or entering the danger zone without the shift having
// happened -- and the reader had only the comments to tell them the order was
// load-bearing rather than incidental.
//
// Each phase is now a function whose argument is the previous phase's return
// value, so the correct order is the only order that compiles, and the values
// are moved rather than borrowed, so no phase can be re-entered. Resume does
// not weaken this: finding a checkpoint never lets a phase be skipped, it lets
// the witness be constructed from the record of the run that already did the
// work.

/// Proof that the cluster data occupies its post-resize sectors.
#[must_use]
struct DataShifted {
    clusters_relocated: usize,
}

impl DataShifted {
    /// An earlier run completed the shift and recorded it in the checkpoint.
    ///
    /// This is the only way to obtain the witness without doing the work, and
    /// it exists because redoing the work would be destructive rather than
    /// merely wasteful: the shift walks clusters downward overwriting as it
    /// goes, so a second pass reads sources the first pass already clobbered
    /// ([F-R1], [F-R3]).
    fn recorded_by_an_earlier_run(clusters_relocated: usize) -> Self {
        Self { clusters_relocated }
    }
}

/// Proof that the FAT extension is on disk and durable -- and that the primary
/// boot sector is, right now, invalidated.
///
/// That second fact used to be visible only as a mutation of a local `boot`
/// binding partway down the function, so "is this filesystem currently marked
/// unusable?" could be answered only by reading the control flow around you.
/// Holding this value *is* the answer: the device carries a filesystem marked
/// unusable, and whoever holds it owes the commit a restored signature.
#[must_use]
struct FatExtended {
    boot: BootSector,
}

impl FatExtended {
    /// An earlier run wrote the extension and recorded it in the checkpoint.
    ///
    /// The `DataShifted` is consumed rather than inspected: a resume may skip
    /// the work of a phase, but it may not skip the phase's precondition, so
    /// this witness is still unobtainable without one.
    fn recorded_by_an_earlier_run(boot: BootSector, _shifted: DataShifted) -> Self {
        Self { boot }
    }
}

/// What the FAT phase left on the device, and so what the metadata commit is
/// about to publish.
#[must_use]
enum FatOnDisk {
    /// No extension was written and the boot sector was never invalidated,
    /// because the FAT is not growing, or because this is a dry run, which
    /// commits nothing. A real resize with a growing FAT always goes through
    /// the FAT phase, even with no cluster data to shift ([F-20]).
    NotWritten(BootSector),
    /// The extension is written and flushed, and the boot sector is invalidated.
    Written(FatExtended),
}

impl FatOnDisk {
    /// The boot sector the commit builds the new geometry from.
    ///
    /// Its signature is valid in the `NotWritten` case and invalidated in the
    /// `Written` case. The commit restores it unconditionally, which is why it
    /// does not need to know which it got -- and why restoring an already-valid
    /// signature has to stay idempotent.
    fn into_boot(self) -> BootSector {
        match self {
            Self::NotWritten(boot) => boot,
            Self::Written(FatExtended { boot }) => boot,
        }
    }
}

/// The recovery record of one resize: where its checkpoint lives, the FSInfo
/// sector that marks that location, and the checkpoint an interrupted run
/// left, if this run resumes one ([F-03]).
struct ResizeRecord {
    slot: CheckpointSlot,
    fsinfo_sector: u16,
    interrupted: Option<ResizeCheckpoint>,
}

/// Everything read off the device before any of it is modified.
struct Prepared {
    device: Device,
    boot: BootSector,
    fsinfo: FSInfo,
    backup_sector: u16,
    calculation: SizeCalculation,
    fat: Vec<u32>,
    record: ResizeRecord,
}

/// Main resize function with crash-safe checkpoint support.
///
/// Reports nothing; [`resize_fat32_with_events`] is the same resize with a
/// callback for its progress.
pub fn resize_fat32(options: ResizeOptions) -> Result<ResizeResult> {
    resize_fat32_with_events(options, &mut |_| {})
}

/// [`resize_fat32`], reporting each step and, with
/// [`ResizeOptions::verbose`], the detail of the resize to `on_event` as it
/// happens ([F-08]). The library itself prints nothing.
pub fn resize_fat32_with_events(
    options: ResizeOptions,
    on_event: &mut dyn FnMut(&ResizeEvent),
) -> Result<ResizeResult> {
    let mut report = Reporter::new(on_event, options.is_verbose());
    let Prepared {
        device,
        boot,
        mut fsinfo,
        backup_sector,
        calculation,
        fat,
        record,
    } = prepare_resize(&options, &mut report)?;

    report.detail(ResizeEvent::Layout {
        old_total_sectors: calculation.old_total_sectors,
        old_fat_size: calculation.old_fat_size,
        old_data_clusters: boot.data_clusters(),
        new_total_sectors: calculation.new_total_sectors,
        new_fat_size: calculation.new_fat_size,
        new_data_clusters: calculation.new_data_clusters,
        fat_needs_growth: calculation.fat_needs_growth,
    });

    fsinfo.set_free_count(free_clusters_after_resize(&boot, &calculation, &fat));
    // The commit writes FSInfo without the checkpoint mark, which retires it
    // ([F-03]).
    fsinfo.set_resize_record_sector(None);

    let old_size_bytes = calculation.old_total_sectors as u64 * boot.bytes_per_sector() as u64;
    let new_size_bytes = calculation.new_total_sectors as u64 * boot.bytes_per_sector() as u64;

    // Phases 0 and 1. `boot` is handed over by value and comes back inside the
    // `FatOnDisk`, so there is exactly one live boot sector at any point and no
    // path can commit one the FAT phase did not see.
    let (fat_on_disk, clusters_relocated) = run_fat_phases(
        &device,
        boot,
        &calculation,
        &fat,
        &record,
        &options,
        &mut report,
    )?;

    // Phase 2.
    if !options.is_dry_run() {
        commit_new_geometry(
            &device,
            fat_on_disk,
            &calculation,
            &fsinfo,
            backup_sector,
            &record,
            &mut report,
        )?;
    } else {
        report.step("Dry run: no changes made");
    }

    Ok(ResizeResult {
        old_size_bytes,
        new_size_bytes,
        fat_grew: calculation.fat_needs_growth,
        clusters_relocated,
        old_total_sectors: calculation.old_total_sectors,
        new_total_sectors: calculation.new_total_sectors,
        old_fat_size: calculation.old_fat_size,
        new_fat_size: calculation.new_fat_size,
        operations: report.into_operations(),
    })
}

/// Read and validate everything the resize depends on, before it changes
/// anything.
fn prepare_resize(options: &ResizeOptions, report: &mut Reporter) -> Result<Prepared> {
    // Check if mounted
    check_not_mounted(options.device_path())?;
    report.step("Verified device is not mounted".to_string());

    // Open device. The sector size is established here, before any
    // sector-addressed I/O is possible ([F-R23]).
    let device = open_fat32_device(options.device_path(), !options.is_dry_run())?;
    report.step(format!(
        "Opened device: {}",
        options.device_path().display()
    ));

    // Read boot sector - use recovery mode to allow invalidated signature
    // from an interrupted resize operation
    let boot = read_boot_sector_for_recovery(&device)?;
    report.step(format!(
        "Read boot sector ({}-byte sectors)",
        boot.bytes_per_sector()
    ));

    // Read FSInfo. It comes first because it marks where an interrupted
    // resize left its checkpoint ([F-03]).
    let fsinfo_sector = boot.fs_info_sector();
    let fsinfo = read_fsinfo(&device, fsinfo_sector)?;
    report.step(format!("Read FSInfo from sector {}", fsinfo_sector));

    // Check for incomplete resize operation. A dry run checks too: it only
    // reads, and it has to preview the resume the real run would do, not a
    // fresh resize built from the boot sector a crash left behind ([F-21]).
    let marked_slot = CheckpointSlot::locate(&device, &fsinfo);
    let incomplete_resize = match check_for_incomplete_resize(&device, &boot, marked_slot)? {
        // Every phase that leaves the boot sector valid also leaves the old
        // geometry and volume in it, so a record that disagrees with a valid
        // boot sector is not this filesystem's: typically one left behind by
        // the filesystem that was on the device before it was reformatted.
        // There is nothing to resume, and a fresh resize overwrites it.
        Some(checkpoint) if boot.is_signature_valid() => match checkpoint.check_against(&boot) {
            Ok(()) => Some(checkpoint),
            Err(_) => {
                report.notice(ResizeEvent::StaleCheckpointIgnored {
                    sector: marked_slot.sector(),
                });
                report.step(format!(
                    "Ignored a stale resize checkpoint at sector {}",
                    marked_slot.sector()
                ));
                None
            }
        },
        // Behind an invalidated boot sector the record is the only way back,
        // so one that does not match is refused, never ignored ([F-02]).
        Some(checkpoint) => {
            checkpoint.check_against(&boot)?;
            Some(checkpoint)
        }
        None => None,
    };
    // A resume keeps the slot the interrupted run chose; a fresh run takes
    // the device's last sector, whatever a stale mark says.
    let checkpoint_slot = if incomplete_resize.is_some() {
        marked_slot
    } else {
        CheckpointSlot::at_device_end(&device)
    };

    if let Some(ref checkpoint) = incomplete_resize {
        report.notice(ResizeEvent::Resuming {
            phase: checkpoint.phase.name(),
            dry_run: options.is_dry_run(),
        });
        report.step(format!(
            "Detected incomplete resize at phase {:?}",
            checkpoint.phase
        ));

        // Validate that device size matches checkpoint
        let device_sectors = device.total_sectors();
        if device_sectors < checkpoint.new_total_sectors as u64 {
            return Err(Error::ResizeSizeMismatch(checkpoint.phase as u8));
        }
    }

    // Read backup boot sector. The match check is skipped on a resume and
    // behind an invalidated boot sector, both of which rewrite the backup.
    let backup_sector = boot.backup_boot_sector();
    let backup_boot = read_backup_boot_sector(&device, backup_sector)?;
    // A verified checkpoint means the disagreement may be this tool's own
    // interrupted commit, which writes the backup first and rewrites it on
    // resume ([F-04]).
    if incomplete_resize.is_some() {
        report.step(format!(
            "Backup boot sector at sector {} will be rewritten by the resumed resize",
            backup_sector
        ));
    } else if boot.is_signature_valid() && !boot_sectors_match(&boot, &backup_boot) {
        // [F-R4]: refusing is the default, and the override has to be an
        // explicit act by the caller -- it commits to the primary being the
        // correct copy, and the resize then rewrites the backup from it.
        if !options.ignores_backup_mismatch() {
            return Err(Error::BackupMismatch);
        }
        report.step(format!(
            "Backup boot sector at sector {} does not match the primary; \
             proceeding on the caller's explicit instruction",
            backup_sector
        ));
    } else {
        report.step(format!(
            "Verified backup boot sector at sector {}",
            backup_sector
        ));
    }

    // Calculate new size (use checkpoint values if resuming)
    let device_sectors = device.total_sectors();
    let calculation = if let Some(ref checkpoint) = incomplete_resize {
        // Use checkpoint values for consistency
        SizeCalculation::from_checkpoint(&boot, checkpoint)?
    } else {
        calculate_new_size(&boot, device_sectors)?
    };

    report.step(format!(
        "Calculated resize: {} -> {} sectors",
        calculation.old_total_sectors, calculation.new_total_sectors
    ));

    // Read FAT table
    let fat = read_fat_table(&device, &boot, 0)?;
    report.step(format!("Read FAT table ({} entries)", fat.len()));

    Ok(Prepared {
        device,
        boot,
        fsinfo,
        backup_sector,
        calculation,
        fat,
        record: ResizeRecord {
            slot: checkpoint_slot,
            fsinfo_sector,
            interrupted: incomplete_resize,
        },
    })
}

/// Phases 0 and 1: shift the cluster data, then write the FAT extension.
///
/// Takes the boot sector by value and returns it inside the [`FatOnDisk`], so
/// the caller cannot hold a stale copy across the danger zone, and cannot
/// commit a boot sector this function never saw.
fn run_fat_phases(
    device: &Device,
    boot: BootSector,
    calculation: &SizeCalculation,
    fat: &[u32],
    record: &ResizeRecord,
    options: &ResizeOptions,
    report: &mut Reporter,
) -> Result<(FatOnDisk, usize)> {
    // Derived here rather than passed in: the phase to start from is a fact
    // *about* the checkpoint, and taking both as parameters would let a caller
    // hand over a pair that disagree.
    let starting_phase = record
        .interrupted
        .as_ref()
        .map(|cp| cp.phase)
        .unwrap_or(ResizePhase::Started);

    if !calculation.fat_needs_growth {
        return Ok((FatOnDisk::NotWritten(boot), 0));
    }

    report.step(format!(
        "FAT needs to grow by {} sectors",
        calculation.fat_growth_sectors
    ));

    // The boot sector as it will be committed once the resize completes.
    // The relocator reads the new data-area start straight off it, so the
    // plan cannot disagree with the geometry the filesystem ends up with.
    let new_boot = boot.with_geometry(calculation.new_total_sectors, calculation.new_fat_size);

    // Plan cluster data shift
    let plan = plan_relocation(&boot, &new_boot, fat, calculation.first_affected_cluster)?;

    // An empty plan still goes through both phases: the FAT has to grow and
    // FAT2 has to move even when no cluster data does ([F-20]).
    report.step(format!(
        "Planned data shift for {} clusters ({} bytes)",
        plan.cluster_count(),
        plan.total_bytes
    ));

    report.detail(ResizeEvent::ShiftPlanned {
        clusters: plan.moves.len(),
    });

    if options.is_dry_run() {
        report.step("Dry run: would shift cluster data".to_string());
        return Ok((FatOnDisk::NotWritten(boot), 0));
    }

    // === PHASE 0: Data shift (safe - source preserved) ===
    let shifted = if starting_phase == ResizePhase::Started {
        shift_cluster_data(device, &boot, calculation, &plan, record, report)?
    } else {
        report.step("Skipping data shift (already done)".to_string());
        DataShifted::recorded_by_an_earlier_run(plan.cluster_count())
    };
    let clusters_relocated = shifted.clusters_relocated;

    // === PHASE 1: FAT operations (dangerous - boot sector invalidated) ===
    let extended = if starting_phase <= ResizePhase::DataCopied {
        write_fat_extension(device, boot, calculation, shifted, record.slot, report)?
    } else {
        report.step("Skipping FAT operations (already done)".to_string());
        FatExtended::recorded_by_an_earlier_run(boot, shifted)
    };

    Ok((FatOnDisk::Written(extended), clusters_relocated))
}

/// Phase 0: move the affected clusters forward into the sectors the enlarged
/// FAT will not occupy, recording progress so a crash here can be resumed.
fn shift_cluster_data(
    device: &Device,
    boot: &BootSector,
    calculation: &SizeCalculation,
    plan: &RelocationPlan,
    record: &ResizeRecord,
    report: &mut Reporter,
) -> Result<DataShifted> {
    // How far a previous pass got. A fresh run has moved nothing, so it
    // resumes from the highest cluster in the plan; a resume must read the
    // recorded position, because restarting the shift from the top would
    // re-read source sectors the previous pass overwrote.
    let resume_from = match &record.interrupted {
        None => plan.highest_cluster(),
        Some(cp) => cp.shift_progress.ok_or(Error::ShiftProgressMissing)?,
    };

    // How often progress is recorded is decided by `ShiftProgress::new`,
    // which keeps it within the shift distance ([F-01]).
    //
    // A shift shorter than one cluster overlaps a cluster with itself, so no
    // move may ever be replayed. A single pass is still safe - each cluster is
    // read whole before it is written - so the run proceeds, but nothing is
    // recorded: a later resume then finds no progress record and refuses
    // rather than corrupting.
    let resumable = plan.shift_clusters(boot) > 0;

    // Write initial checkpoint
    let mut checkpoint = calculation.checkpoint(ResizePhase::Started, boot.volume_id());
    if resumable {
        checkpoint.shift_progress = Some(resume_from);
    }
    write_checkpoint(device, record.slot, &checkpoint)?;
    report.step("Wrote checkpoint (phase 0: started)".to_string());

    // [F-03]: the record is durable, so point FSInfo at it before the first
    // move. From here on a rerun finds it whatever size the device has.
    mark_checkpoint_slot(device, record.fsinfo_sector, record.slot)?;
    report.step(format!(
        "Marked checkpoint at sector {} in FSInfo",
        record.slot.sector()
    ));

    maybe_crash_at(CrashPoint::AfterCheckpointStart);

    // Execute data shift, recording how far it has got as it descends so a
    // crash here can be resumed rather than replayed from the beginning.
    let mut records_written = 0u32;
    let mut record_progress = |next_cluster: u32| -> Result<()> {
        if !resumable {
            return Ok(());
        }
        if records_written == 1 {
            maybe_crash_at(CrashPoint::BeforeMidShiftRecord);
        }
        records_written += 1;
        let mut cp = checkpoint.clone();
        cp.shift_progress = Some(next_cluster);
        write_checkpoint(device, record.slot, &cp)
    };
    let mut progress = ShiftProgress::new(plan, boot, resume_from, &mut record_progress);
    execute_relocation(device, boot, plan, &mut progress, report)?;
    let clusters_relocated = plan.cluster_count();
    report.step(format!("Shifted {} clusters forward", clusters_relocated));

    maybe_crash_at(CrashPoint::AfterDataShift);

    // Update checkpoint to phase 1
    let checkpoint = calculation.checkpoint(ResizePhase::DataCopied, boot.volume_id());
    write_checkpoint(device, record.slot, &checkpoint)?;
    report.step("Updated checkpoint (phase 1: data copied)".to_string());

    maybe_crash_at(CrashPoint::AfterCheckpointDataCopied);

    Ok(DataShifted { clusters_relocated })
}

/// Phase 1, the danger zone: mark the filesystem unusable, extend FAT1, mirror
/// it into the remaining FAT copies, and make all of it durable before the
/// checkpoint promises that it is.
///
/// The boot sector goes in valid and comes back invalidated, inside the
/// witness. That is the whole of the danger zone: it opens when this function
/// is called and closes when [`commit_new_geometry`] consumes what it returns,
/// and neither boundary depends on a comment any more ([F-R15]).
fn write_fat_extension(
    device: &Device,
    mut boot: BootSector,
    calculation: &SizeCalculation,
    _shifted: DataShifted,
    checkpoint_slot: CheckpointSlot,
    report: &mut Reporter,
) -> Result<FatExtended> {
    // Invalidate boot sector to prevent other tools from operating
    boot.invalidate_signature();
    write_boot_sector(device, &boot)?;
    device.sync()?;
    report.step("Invalidated boot sector (danger zone)".to_string());

    maybe_crash_at(CrashPoint::AfterBootInvalidate);

    // Both after relocation: the FAT1 extension is zeroed over sectors the
    // data has just moved out of, and the copies take FAT1 as it now stands.
    // The FAT sizes come from the calculation, because `boot` still carries
    // the old geometry until the commit ([F-15]).
    zero_fat_extension(
        device,
        &boot,
        calculation.old_fat_size,
        calculation.new_fat_size,
    )?;
    report.step("Initialized new FAT sectors".to_string());

    copy_fat_to_other_copies(
        device,
        &boot,
        calculation.old_fat_size,
        calculation.new_fat_size,
    )?;
    report.step("Synced FAT copies".to_string());

    // [F-R17]: the checkpoint written below is a promise that the FAT
    // extension is on disk -- a resume reads it and skips the FAT work
    // entirely. Without this barrier both land in one flush window, so the
    // device may commit the checkpoint first; a power loss there leaves
    // recovery refusing to redo work that never reached the platter. Nothing
    // reads the device between the two helpers above, so one barrier covering
    // both is sufficient.
    device.sync()?;
    report.step("Flushed FAT writes".to_string());

    maybe_crash_at(CrashPoint::AfterFatWrite);

    // Update checkpoint to phase 2
    let checkpoint = calculation.checkpoint(ResizePhase::FatWritten, boot.volume_id());
    write_checkpoint(device, checkpoint_slot, &checkpoint)?;
    report.step("Updated checkpoint (phase 2: FAT written)".to_string());

    maybe_crash_at(CrashPoint::AfterCheckpointFatWritten);

    Ok(FatExtended { boot })
}

/// Phase 2: publish the new geometry.
///
/// Consumes the [`FatOnDisk`], which is what closes the danger zone: after
/// this returns, no witness of the FAT phase exists any more, so nothing can
/// commit twice or commit without having gone through phases 0 and 1 first.
fn commit_new_geometry(
    device: &Device,
    fat_on_disk: FatOnDisk,
    calculation: &SizeCalculation,
    fsinfo: &FSInfo,
    backup_sector: u16,
    record: &ResizeRecord,
    report: &mut Reporter,
) -> Result<()> {
    // [F-04]: without a FAT phase there is no checkpoint yet, and a crash
    // between the backup and primary boot sector writes would leave two
    // disagreeing boot sectors and nothing to say why. Record that only the
    // commit remains, so a rerun finishes it. The FAT phase already left
    // this record on the growth path.
    if let FatOnDisk::NotWritten(boot) = &fat_on_disk {
        let checkpoint = calculation.checkpoint(ResizePhase::FatWritten, boot.volume_id());
        write_checkpoint(device, record.slot, &checkpoint)?;
        mark_checkpoint_slot(device, record.fsinfo_sector, record.slot)?;
        report.step("Wrote checkpoint (phase 2: commit pending)".to_string());
    }

    // Update boot sector with new values and restore signature
    let mut boot = fat_on_disk
        .into_boot()
        .with_geometry(calculation.new_total_sectors, calculation.new_fat_size);
    boot.restore_signature(); // Restore 0xAA55 signature

    // [F-R26]: the primary boot sector is written LAST, and is the single
    // commit point for the whole resize. Everything the new geometry
    // depends on -- the backup boot sector and FSInfo -- is published and
    // flushed first, while the primary still describes the old layout (or,
    // on the FAT-growth path, carries an invalidated signature and a live
    // checkpoint). A crash anywhere before the primary lands therefore
    // leaves a filesystem that still reads as its old self, which is
    // recoverable; a crash after it lands leaves one whose supporting
    // structures are already correct.
    //
    // The previous order wrote the primary first, which opened a window
    // where a valid new-geometry primary sat against a stale old-geometry
    // backup. That combination is reported as `Error::BackupMismatch` and
    // refused. [F-R4] has since given that refusal a real escape
    // (`ResizeOptions::ignore_backup_mismatch`), but the escape asks the
    // user to vouch for the primary, so leaving the window open would have
    // meant asking them to vouch for a state this code created itself.
    write_backup_boot_sector(device, &boot, backup_sector)?;
    report.step("Updated backup boot sector".to_string());

    // The free count was set by the caller from the FAT; see
    // [`free_clusters_after_resize`].
    let new_free = fsinfo.free_count();
    write_fsinfo(device, fsinfo, record.fsinfo_sector)?;
    report.step(format!("Updated FSInfo (free clusters: {})", new_free));

    // [F-R26]: the supporting structures must be durable before the commit
    // point publishes a geometry that assumes them.
    device.sync()?;
    report.step("Flushed supporting structures".to_string());

    maybe_crash_at(CrashPoint::BeforePrimaryBootWrite);

    write_boot_sector(device, &boot)?;
    report.step("Updated boot sector (signature restored)".to_string());

    // [F-R18]: clearing the checkpoint retires the only record that can
    // recover this filesystem, so everything it protects must be durable
    // first. Unsynced, the erase may reach the platter before the restored
    // boot sector, and a power loss there leaves an invalidated signature
    // with no checkpoint -- the one combination
    // `check_for_incomplete_resize` reports as `InvalidatedFilesystem` and
    // refuses to touch.
    device.sync()?;
    report.step("Flushed new geometry".to_string());

    // Clear checkpoint
    clear_checkpoint(device, record.slot)?;
    report.step("Cleared checkpoint".to_string());

    // Final sync
    device.sync()?;
    report.step("Synced changes to disk".to_string());

    Ok(())
}

/// The free-cluster count of the resized filesystem.
///
/// [F-04]: the count is derived from the FAT, never from the free count
/// already on disk. Adding the new clusters to the on-disk hint counted them
/// twice whenever the commit ran again after FSInfo had landed: a
/// `FatWritten` resume, or the `--ignore-backup-mismatch` rerun after a crash
/// between the FSInfo write and the primary boot sector's. `fat` is read with
/// the old geometry and no phase changes its entries, so this is the same
/// number on every run.
fn free_clusters_after_resize(
    boot: &BootSector,
    calculation: &SizeCalculation,
    fat: &[u32],
) -> u32 {
    // The old geometry came off a filesystem that validated, so it fits;
    // falling back to the new count yields no added clusters rather than
    // inventing free space.
    let old_data_clusters = boot
        .with_geometry(calculation.old_total_sectors, calculation.old_fat_size)
        .data_clusters()
        .unwrap_or(calculation.new_data_clusters);
    let old_free = (2..old_data_clusters.saturating_add(2) as usize)
        .filter(|&cluster| fat.get(cluster).copied().is_some_and(fat_entry::is_free))
        .count() as u32;
    let additional_clusters = calculation
        .new_data_clusters
        .saturating_sub(old_data_clusters);
    old_free.saturating_add(additional_clusters)
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
