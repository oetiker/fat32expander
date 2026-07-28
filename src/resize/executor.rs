use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::{
    boot_sectors_match, read_backup_boot_sector, read_boot_sector, read_boot_sector_for_recovery,
    read_fat_table, read_fsinfo, write_backup_boot_sector, write_boot_sector, write_fsinfo,
    BootSector, FSInfo,
};
use crate::resize::calculator::{affected_cluster_range, calculate_new_size, SizeCalculation};
use crate::resize::relocator::{execute_relocation, plan_relocation, ShiftProgress};
use crate::system::check_not_mounted;

// ===== Fault Injection for Testing =====
//
// Only available when compiled with --features fault-injection
//
// Set FAT32_CRASH_AT environment variable to simulate crashes at specific points:
//   - "after_checkpoint_start" - after writing phase 0 checkpoint, before data shift
//   - "after_data_shift" - after data shift, before phase 1 checkpoint
//   - "after_checkpoint_data_copied" - after phase 1 checkpoint, before boot invalidation
//   - "after_boot_invalidate" - after boot sector invalidated, before FAT operations
//   - "after_fat_write" - after FAT written, before phase 2 checkpoint
//   - "after_checkpoint_fat_written" - after phase 2 checkpoint, before boot restore
//
// Build: cargo build --release --features fault-injection
// Usage: FAT32_CRASH_AT=after_boot_invalidate fat32expander resize image.img

/// Check if we should crash at a specific point (for testing crash recovery)
/// This function only does anything when compiled with the fault-injection feature
#[cfg(feature = "fault-injection")]
fn maybe_crash_at(point: &str) {
    if let Ok(crash_point) = std::env::var("FAT32_CRASH_AT") {
        if crash_point == point {
            eprintln!("FAULT INJECTION: Simulating crash at '{}'", point);
            std::process::exit(137); // Exit like killed by SIGKILL
        }
    }
}

/// No-op version for production builds
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
fn maybe_crash_at(_point: &str) {
    // No-op in production builds
}

// ===== Crash-Safe Checkpoint Support =====

/// Magic bytes for resize checkpoint identification
const CHECKPOINT_MAGIC: &[u8; 8] = b"FAT32RSZ";

/// Current checkpoint version
const CHECKPOINT_VERSION: u8 = 1;

/// Magic bytes for the shift-progress record.
///
/// The record is appended *after* the version-1 checkpoint body rather than
/// folded into it, so the first 32 bytes stay byte-for-byte what version 1
/// always was. A build that predates the record ignores the tail and still
/// reads the checkpoint; a build that expects it and finds none refuses to
/// resume a data shift instead of guessing how far the previous pass got.
const PROGRESS_MAGIC: &[u8; 4] = b"PRG1";

/// Offset of the shift-progress record within the checkpoint sector
const PROGRESS_OFFSET: usize = 32;

/// magic (4) + next_cluster (4) + CRC32 (4)
const PROGRESS_SIZE: usize = 12;

/// Record shift progress at most this often, in moves. The effective interval
/// is also capped by the shift distance in clusters - see [`ShiftProgress`].
const PROGRESS_RECORD_EVERY: u32 = 1024;

/// Resize phase values
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ResizePhase {
    /// Resize started, data shift may be in progress. Boot sector still valid.
    Started = 0,
    /// Data copied, entering danger zone. Boot sector invalidated.
    DataCopied = 1,
    /// FAT written, completing metadata. Boot sector about to be restored.
    FatWritten = 2,
}

impl ResizePhase {
    fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::Started),
            1 => Some(Self::DataCopied),
            2 => Some(Self::FatWritten),
            _ => None,
        }
    }
}

/// Checkpoint stored in new space for crash recovery
///
/// This is written to sector `old_total_sectors` (first sector of new space)
/// and allows resuming an interrupted resize operation.
#[derive(Debug, Clone)]
pub struct ResizeCheckpoint {
    /// Current resize phase
    pub phase: ResizePhase,
    /// Original filesystem total sectors (for validation)
    pub old_total_sectors: u32,
    /// Target filesystem total sectors
    pub new_total_sectors: u32,
    /// Original FAT size in sectors
    pub old_fat_size: u32,
    /// New FAT size in sectors
    pub new_fat_size: u32,
    /// Highest cluster still to be moved by the data shift.
    ///
    /// `None` means no progress record was found, which is not the same as
    /// "nothing moved yet": a shift may have run to an unknown point. Only a
    /// recorded `Some` makes a phase-`Started` resume safe.
    pub shift_progress: Option<u32>,
}

impl ResizeCheckpoint {
    /// Checkpoint size in bytes (without CRC)
    const DATA_SIZE: usize = 8 + 1 + 1 + 2 + 4 + 4 + 4 + 4; // 28 bytes

    /// Create a new checkpoint
    pub fn new(
        phase: ResizePhase,
        old_total_sectors: u32,
        new_total_sectors: u32,
        old_fat_size: u32,
        new_fat_size: u32,
    ) -> Self {
        Self {
            phase,
            old_total_sectors,
            new_total_sectors,
            old_fat_size,
            new_fat_size,
            shift_progress: None,
        }
    }

    /// Attach a shift-progress record naming the next cluster still to move.
    pub fn with_shift_progress(mut self, next_cluster: u32) -> Self {
        self.shift_progress = Some(next_cluster);
        self
    }

    /// Serialize checkpoint to bytes with specified sector size
    pub fn to_bytes(&self, sector_size: usize) -> Vec<u8> {
        let mut data = vec![0u8; sector_size];

        // Magic (8 bytes)
        data[0..8].copy_from_slice(CHECKPOINT_MAGIC);

        // Version (1 byte)
        data[8] = CHECKPOINT_VERSION;

        // Phase (1 byte)
        data[9] = self.phase as u8;

        // Padding (2 bytes)
        data[10] = 0;
        data[11] = 0;

        // old_total_sectors (4 bytes)
        data[12..16].copy_from_slice(&self.old_total_sectors.to_le_bytes());

        // new_total_sectors (4 bytes)
        data[16..20].copy_from_slice(&self.new_total_sectors.to_le_bytes());

        // old_fat_size (4 bytes)
        data[20..24].copy_from_slice(&self.old_fat_size.to_le_bytes());

        // new_fat_size (4 bytes)
        data[24..28].copy_from_slice(&self.new_fat_size.to_le_bytes());

        // CRC32 of data (4 bytes at offset 28)
        let crc = crc32fast::hash(&data[0..Self::DATA_SIZE]);
        data[28..32].copy_from_slice(&crc.to_le_bytes());

        // Shift-progress record, appended after the version-1 body so that
        // body's layout and CRC are untouched.
        if let Some(next_cluster) = self.shift_progress {
            if sector_size >= PROGRESS_OFFSET + PROGRESS_SIZE {
                let p = PROGRESS_OFFSET;
                data[p..p + 4].copy_from_slice(PROGRESS_MAGIC);
                data[p + 4..p + 8].copy_from_slice(&next_cluster.to_le_bytes());
                let crc = crc32fast::hash(&data[p..p + 8]);
                data[p + 8..p + 12].copy_from_slice(&crc.to_le_bytes());
            }
        }

        data
    }

    /// Parse the optional shift-progress record trailing the checkpoint body.
    ///
    /// A record whose magic is absent means "no progress was recorded"; a
    /// record whose magic is present but whose CRC fails is damage, and is
    /// reported rather than silently treated as absent.
    fn parse_shift_progress(data: &[u8]) -> Result<Option<u32>> {
        let p = PROGRESS_OFFSET;
        if data.len() < p + PROGRESS_SIZE {
            return Ok(None);
        }
        if &data[p..p + 4] != PROGRESS_MAGIC {
            return Ok(None);
        }

        let stored = u32::from_le_bytes([data[p + 8], data[p + 9], data[p + 10], data[p + 11]]);
        if stored != crc32fast::hash(&data[p..p + 8]) {
            return Err(Error::CheckpointCorrupted);
        }

        Ok(Some(u32::from_le_bytes([
            data[p + 4],
            data[p + 5],
            data[p + 6],
            data[p + 7],
        ])))
    }

    /// Parse checkpoint from bytes (sector size independent - only first 32 bytes matter)
    pub fn from_bytes(data: &[u8]) -> Result<Option<Self>> {
        // Need at least 32 bytes for checkpoint data
        if data.len() < 32 {
            return Ok(None);
        }

        // Check magic
        if &data[0..8] != CHECKPOINT_MAGIC {
            return Ok(None);
        }

        // Check version
        let version = data[8];
        if version != CHECKPOINT_VERSION {
            return Ok(None);
        }

        // Verify CRC
        let stored_crc = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        let computed_crc = crc32fast::hash(&data[0..Self::DATA_SIZE]);
        if stored_crc != computed_crc {
            return Err(Error::CheckpointCorrupted);
        }

        // Parse phase
        let Some(phase) = ResizePhase::from_u8(data[9]) else {
            return Err(Error::CheckpointCorrupted);
        };

        // Parse fields
        let old_total_sectors = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let new_total_sectors = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let old_fat_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        let new_fat_size = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);

        Ok(Some(Self {
            phase,
            old_total_sectors,
            new_total_sectors,
            old_fat_size,
            new_fat_size,
            shift_progress: Self::parse_shift_progress(data)?,
        }))
    }
}

/// Write checkpoint to the last sector of the device
///
/// We use the last sector of the device (not the first sector of new space)
/// because when data is shifted forward during FAT growth, some clusters
/// will be written to sectors starting at old_total_sectors. Using the last
/// sector ensures the checkpoint won't be overwritten by shifted cluster data.
fn write_checkpoint(device: &Device, checkpoint: &ResizeCheckpoint) -> Result<()> {
    let sector = device.total_sectors() - 1;
    let sector_size = device.sector_size() as usize;
    device.write_sector(sector, &checkpoint.to_bytes(sector_size))?;
    device.sync()?;
    Ok(())
}

/// Read checkpoint from the last sector of the device
fn read_checkpoint(device: &Device, boot: &BootSector) -> Result<Option<ResizeCheckpoint>> {
    // Only check if device is larger than filesystem
    if device.total_sectors() <= boot.total_sectors() as u64 {
        return Ok(None);
    }

    let checkpoint_sector = device.total_sectors() - 1;
    let data = device.read_sector(checkpoint_sector)?;
    ResizeCheckpoint::from_bytes(&data)
}

/// Clear checkpoint by zeroing the last sector of the device
fn clear_checkpoint(device: &Device) -> Result<()> {
    let sector = device.total_sectors() - 1;
    let zeros = vec![0u8; device.sector_size() as usize];
    device.write_sector(sector, &zeros)?;
    Ok(())
}

/// Check for incomplete resize operation and return checkpoint if found
fn check_for_incomplete_resize(
    device: &Device,
    boot: &BootSector,
) -> Result<Option<ResizeCheckpoint>> {
    // First check if boot sector is invalidated
    if !boot.is_signature_valid() {
        // Boot sector was invalidated - we MUST find a valid checkpoint
        // Checkpoint is at last sector of device
        if device.total_sectors() <= boot.total_sectors() as u64 {
            return Err(Error::InvalidatedFilesystem);
        }

        let checkpoint_sector = device.total_sectors() - 1;
        let data = device.read_sector(checkpoint_sector)?;
        match ResizeCheckpoint::from_bytes(&data)? {
            Some(cp) => Ok(Some(cp)),
            None => Err(Error::InvalidatedFilesystem),
        }
    } else {
        // Boot sector valid - check for checkpoint anyway (phase 0 crash)
        read_checkpoint(device, boot)
    }
}

/// Options for the resize operation
#[derive(Debug, Clone)]
pub struct ResizeOptions {
    device_path: std::path::PathBuf,
    dry_run: bool,
    verbose: bool,
}

impl ResizeOptions {
    /// Create new resize options for the given device path
    pub fn new(device_path: impl AsRef<std::path::Path>) -> Self {
        Self {
            device_path: device_path.as_ref().to_path_buf(),
            dry_run: false,
            verbose: false,
        }
    }

    /// Enable or disable dry run mode (don't make changes)
    pub fn dry_run(mut self, enable: bool) -> Self {
        self.dry_run = enable;
        self
    }

    /// Enable or disable verbose output
    pub fn verbose(mut self, enable: bool) -> Self {
        self.verbose = enable;
        self
    }

    /// Get the device path
    pub fn device_path(&self) -> &std::path::Path {
        &self.device_path
    }

    /// Check if dry run mode is enabled
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Check if verbose mode is enabled
    pub fn is_verbose(&self) -> bool {
        self.verbose
    }
}

/// Result of a resize operation
#[derive(Debug)]
pub struct ResizeResult {
    /// Old filesystem size in bytes
    pub old_size_bytes: u64,
    /// New filesystem size in bytes
    pub new_size_bytes: u64,
    /// Whether FAT tables grew
    pub fat_grew: bool,
    /// Number of clusters relocated
    pub clusters_relocated: usize,
    /// Detailed calculation results
    pub calculation: SizeCalculation,
    /// List of operations performed (for logging)
    pub operations: Vec<String>,
}

/// Main resize function with crash-safe checkpoint support
pub fn resize_fat32(options: ResizeOptions) -> Result<ResizeResult> {
    let mut operations = Vec::new();

    // Check if mounted
    check_not_mounted(options.device_path())?;
    operations.push("Verified device is not mounted".to_string());

    // Open device
    let mut device = if options.is_dry_run() {
        Device::open_readonly(options.device_path())?
    } else {
        Device::open(options.device_path())?
    };
    operations.push(format!(
        "Opened device: {}",
        options.device_path().display()
    ));

    // Read boot sector - use recovery mode to allow invalidated signature
    // from an interrupted resize operation
    // This also bootstraps the device sector size from the boot sector
    let mut boot = read_boot_sector_for_recovery(&mut device)?;
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

        SizeCalculation {
            old_total_sectors: checkpoint.old_total_sectors,
            new_total_sectors: checkpoint.new_total_sectors,
            old_fat_size: checkpoint.old_fat_size,
            new_fat_size: checkpoint.new_fat_size,
            new_data_clusters: boot
                .with_geometry(checkpoint.new_total_sectors, checkpoint.new_fat_size)
                .data_clusters(),
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
        let old_data_clusters = boot
            .with_geometry(calculation.old_total_sectors, calculation.old_fat_size)
            .data_clusters();
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
    eprintln!("  Data clusters: {}", boot.data_clusters());
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

/// Get information about a FAT32 filesystem without modifying it
pub fn get_fs_info(device_path: impl AsRef<std::path::Path>) -> Result<FSInfoReport> {
    let device_path = device_path.as_ref();
    let mut device = Device::open_readonly(device_path)?;
    let boot = read_boot_sector(&mut device)?;

    let backup_sector = boot.backup_boot_sector();
    let backup_boot = read_backup_boot_sector(&device, backup_sector)?;
    let backup_matches = boot_sectors_match(&boot, &backup_boot);

    let fsinfo_sector = boot.fs_info_sector();
    let fsinfo = read_fsinfo(&device, fsinfo_sector)?;

    let device_sectors = device.total_sectors();
    let current_sectors = boot.total_sectors();
    let can_grow = device_sectors > current_sectors as u64;

    let max_new_size = if can_grow {
        Some(device_sectors * boot.bytes_per_sector() as u64)
    } else {
        None
    };

    Ok(FSInfoReport {
        device_path: device_path.to_path_buf(),
        bytes_per_sector: boot.bytes_per_sector(),
        sectors_per_cluster: boot.sectors_per_cluster(),
        reserved_sectors: boot.reserved_sectors(),
        num_fats: boot.num_fats(),
        fat_size_sectors: boot.fat_size(),
        total_sectors: boot.total_sectors(),
        data_clusters: boot.data_clusters(),
        root_cluster: boot.root_cluster(),
        fsinfo_sector: boot.fs_info_sector(),
        backup_boot_sector: boot.backup_boot_sector(),
        free_clusters: fsinfo.free_count(),
        backup_matches,
        device_sectors,
        can_grow,
        current_size_bytes: current_sectors as u64 * boot.bytes_per_sector() as u64,
        max_new_size_bytes: max_new_size,
    })
}

/// Report about a FAT32 filesystem
#[derive(Debug)]
pub struct FSInfoReport {
    pub device_path: std::path::PathBuf,
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub num_fats: u8,
    pub fat_size_sectors: u32,
    pub total_sectors: u32,
    pub data_clusters: u32,
    pub root_cluster: u32,
    pub fsinfo_sector: u16,
    pub backup_boot_sector: u16,
    pub free_clusters: u32,
    pub backup_matches: bool,
    pub device_sectors: u64,
    pub can_grow: bool,
    pub current_size_bytes: u64,
    pub max_new_size_bytes: Option<u64>,
}

impl std::fmt::Display for FSInfoReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "FAT32 Filesystem Information")?;
        writeln!(f, "============================")?;
        writeln!(f, "Device: {}", self.device_path.display())?;
        writeln!(f)?;
        writeln!(f, "Geometry:")?;
        writeln!(f, "  Bytes per sector: {}", self.bytes_per_sector)?;
        writeln!(f, "  Sectors per cluster: {}", self.sectors_per_cluster)?;
        writeln!(
            f,
            "  Bytes per cluster: {}",
            self.bytes_per_sector as u32 * self.sectors_per_cluster as u32
        )?;
        writeln!(f)?;
        writeln!(f, "Layout:")?;
        writeln!(f, "  Reserved sectors: {}", self.reserved_sectors)?;
        writeln!(f, "  Number of FATs: {}", self.num_fats)?;
        writeln!(f, "  FAT size (sectors): {}", self.fat_size_sectors)?;
        writeln!(f, "  Total sectors: {}", self.total_sectors)?;
        writeln!(f, "  Data clusters: {}", self.data_clusters)?;
        writeln!(f)?;
        writeln!(f, "Special sectors:")?;
        writeln!(f, "  Root directory cluster: {}", self.root_cluster)?;
        writeln!(f, "  FSInfo sector: {}", self.fsinfo_sector)?;
        writeln!(f, "  Backup boot sector: {}", self.backup_boot_sector)?;
        writeln!(
            f,
            "  Backup matches primary: {}",
            if self.backup_matches { "Yes" } else { "NO" }
        )?;
        writeln!(f)?;
        writeln!(f, "Usage:")?;
        if self.free_clusters == FSInfo::UNKNOWN_FREE {
            writeln!(f, "  Free clusters: Unknown")?;
        } else {
            let free_bytes = self.free_clusters as u64
                * self.bytes_per_sector as u64
                * self.sectors_per_cluster as u64;
            writeln!(
                f,
                "  Free clusters: {} ({} bytes)",
                self.free_clusters, free_bytes
            )?;
        }
        writeln!(f)?;
        writeln!(f, "Size:")?;
        writeln!(
            f,
            "  Current size: {} bytes ({:.2} MB)",
            self.current_size_bytes,
            self.current_size_bytes as f64 / (1024.0 * 1024.0)
        )?;
        writeln!(f, "  Device sectors: {}", self.device_sectors)?;
        writeln!(
            f,
            "  Can grow: {}",
            if self.can_grow { "Yes" } else { "No" }
        )?;
        if let Some(max_size) = self.max_new_size_bytes {
            writeln!(
                f,
                "  Max new size: {} bytes ({:.2} MB)",
                max_size,
                max_size as f64 / (1024.0 * 1024.0)
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Build a FAT32 boot sector with a standard geometry.
    fn test_boot_sector(total_sectors: u32, fat_size: u32) -> BootSector {
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
                expected,
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

    // ===== Checkpoint safety net =====
    //
    // The checkpoint is the only durable record of an interrupted resize, and
    // its layout is spelled out as bare byte offsets in `to_bytes` and again in
    // `from_bytes`. Nothing exercised either side before these tests, so a
    // mismatch between the two would have corrupted crash recovery silently.
    // Any change to the record - a new field, a version bump - has to keep
    // these passing.

    /// A checkpoint carrying values that are distinct from one another, so a
    /// field written to or read from the wrong offset cannot go unnoticed.
    fn test_checkpoint(phase: ResizePhase) -> ResizeCheckpoint {
        ResizeCheckpoint::new(phase, 0x1111_2222, 0x3333_4444, 0x5555_6666, 0x7777_8888)
    }

    fn assert_checkpoint_eq(actual: &ResizeCheckpoint, expected: &ResizeCheckpoint) {
        assert_eq!(actual.phase, expected.phase);
        assert_eq!(actual.old_total_sectors, expected.old_total_sectors);
        assert_eq!(actual.new_total_sectors, expected.new_total_sectors);
        assert_eq!(actual.old_fat_size, expected.old_fat_size);
        assert_eq!(actual.new_fat_size, expected.new_fat_size);
    }

    /// Every phase and every field must survive a serialize/parse round trip.
    #[test]
    fn checkpoint_round_trips_every_phase_and_field() {
        for phase in [
            ResizePhase::Started,
            ResizePhase::DataCopied,
            ResizePhase::FatWritten,
        ] {
            let checkpoint = test_checkpoint(phase);
            let bytes = checkpoint.to_bytes(512);

            let parsed = ResizeCheckpoint::from_bytes(&bytes)
                .unwrap_or_else(|e| panic!("phase {:?} failed to parse: {:?}", phase, e))
                .unwrap_or_else(|| panic!("phase {:?} parsed as absent", phase));

            assert_checkpoint_eq(&parsed, &checkpoint);
        }
    }

    /// Field values must land at the offsets the parser reads them from. This
    /// asserts the on-disk layout directly rather than comparing the two
    /// implementations against each other, so a matching pair of wrong offsets
    /// is still caught.
    #[test]
    fn checkpoint_writes_the_documented_byte_layout() {
        let bytes = test_checkpoint(ResizePhase::DataCopied).to_bytes(512);

        assert_eq!(&bytes[0..8], CHECKPOINT_MAGIC);
        assert_eq!(bytes[8], CHECKPOINT_VERSION);
        assert_eq!(bytes[9], ResizePhase::DataCopied as u8);
        assert_eq!(&bytes[10..12], &[0, 0], "padding must stay zeroed");
        assert_eq!(&bytes[12..16], &0x1111_2222u32.to_le_bytes());
        assert_eq!(&bytes[16..20], &0x3333_4444u32.to_le_bytes());
        assert_eq!(&bytes[20..24], &0x5555_6666u32.to_le_bytes());
        assert_eq!(&bytes[24..28], &0x7777_8888u32.to_le_bytes());
        assert_eq!(
            &bytes[28..32],
            &crc32fast::hash(&bytes[0..28]).to_le_bytes()
        );
    }

    /// A corrupted record must be reported, never parsed into a plausible
    /// resume plan. Each data byte is covered, so a field left outside the CRC
    /// range would be caught.
    #[test]
    fn checkpoint_rejects_corruption_in_any_covered_byte() {
        let good = test_checkpoint(ResizePhase::FatWritten).to_bytes(512);

        for offset in 0..ResizeCheckpoint::DATA_SIZE {
            let mut corrupt = good.clone();
            corrupt[offset] ^= 0x01;

            match ResizeCheckpoint::from_bytes(&corrupt) {
                // Magic and version are checked before the CRC; a sector that
                // fails those is foreign data, not a damaged checkpoint.
                Ok(None) if offset < 9 => {}
                Err(Error::CheckpointCorrupted) => {}
                other => panic!(
                    "flipping byte {} should have been rejected, got {:?}",
                    offset, other
                ),
            }
        }
    }

    /// A phase byte outside the known range is corruption, not a newer format:
    /// resuming would compare it with `<=` against the real phases and skip
    /// work that never ran.
    #[test]
    fn checkpoint_rejects_an_unknown_phase_byte() {
        let mut bytes = test_checkpoint(ResizePhase::Started).to_bytes(512);
        bytes[9] = 3;
        // Re-stamp the CRC so the phase check is what rejects it, not the CRC.
        let crc = crc32fast::hash(&bytes[0..ResizeCheckpoint::DATA_SIZE]);
        bytes[28..32].copy_from_slice(&crc.to_le_bytes());

        assert!(matches!(
            ResizeCheckpoint::from_bytes(&bytes),
            Err(Error::CheckpointCorrupted)
        ));
    }

    /// A sector that is not one of our checkpoints means "no resize in
    /// progress" - it must not raise an error, or an ordinary resize over
    /// arbitrary trailing data would refuse to run.
    #[test]
    fn checkpoint_reads_foreign_sectors_as_absent() {
        let mut wrong_magic = test_checkpoint(ResizePhase::Started).to_bytes(512);
        wrong_magic[0] = b'X';

        let mut wrong_version = test_checkpoint(ResizePhase::Started).to_bytes(512);
        wrong_version[8] = CHECKPOINT_VERSION.wrapping_add(1);

        for (label, data) in [
            ("all zeroes", vec![0u8; 512]),
            ("arbitrary data", vec![0xA5u8; 512]),
            ("truncated", vec![0u8; 31]),
            ("wrong magic", wrong_magic),
            ("wrong version", wrong_version),
        ] {
            let parsed = ResizeCheckpoint::from_bytes(&data)
                .unwrap_or_else(|e| panic!("{} returned an error: {:?}", label, e));
            assert!(parsed.is_none(), "{} parsed as a checkpoint", label);
        }
    }

    /// The record occupies a whole sector but its content is sector-size
    /// independent, so a checkpoint written on one geometry stays readable if
    /// the sector size is re-derived differently on the recovery run.
    #[test]
    fn checkpoint_content_is_independent_of_sector_size() {
        let checkpoint = test_checkpoint(ResizePhase::DataCopied);

        let small = checkpoint.to_bytes(512);
        let large = checkpoint.to_bytes(4096);

        assert_eq!(small.len(), 512);
        assert_eq!(large.len(), 4096);
        assert_eq!(small[0..32], large[0..32]);
        assert!(
            large[32..].iter().all(|&b| b == 0),
            "trailing sector space must stay zeroed"
        );

        assert_checkpoint_eq(
            &ResizeCheckpoint::from_bytes(&large).unwrap().unwrap(),
            &checkpoint,
        );
    }

    /// `Started <= DataCopied <= FatWritten` is what the resume branches in
    /// `resize_fat32` compare against to decide which work to skip. Reordering
    /// the variants would silently re-run or skip a phase.
    #[test]
    fn resize_phases_order_from_earliest_to_latest() {
        assert!(ResizePhase::Started < ResizePhase::DataCopied);
        assert!(ResizePhase::DataCopied < ResizePhase::FatWritten);

        assert_eq!(ResizePhase::from_u8(0), Some(ResizePhase::Started));
        assert_eq!(ResizePhase::from_u8(1), Some(ResizePhase::DataCopied));
        assert_eq!(ResizePhase::from_u8(2), Some(ResizePhase::FatWritten));
        assert_eq!(ResizePhase::from_u8(3), None);
        assert_eq!(ResizePhase::from_u8(255), None);
    }

    /// The progress record has to survive the same round trip as the rest.
    #[test]
    fn checkpoint_round_trips_a_shift_progress_record() {
        let checkpoint = test_checkpoint(ResizePhase::Started).with_shift_progress(4321);
        let parsed = ResizeCheckpoint::from_bytes(&checkpoint.to_bytes(512))
            .unwrap()
            .unwrap();

        assert_checkpoint_eq(&parsed, &checkpoint);
        assert_eq!(parsed.shift_progress, Some(4321));
    }

    /// Appending the progress record must not disturb the version-1 body. A
    /// build that predates the record has to keep reading these checkpoints,
    /// which is the whole reason the record lives past byte 32 instead of
    /// inside the body.
    #[test]
    fn progress_record_leaves_the_version_1_body_untouched() {
        let plain = test_checkpoint(ResizePhase::FatWritten);
        let with_progress = plain.clone().with_shift_progress(4321);

        assert_eq!(
            plain.to_bytes(512)[0..32],
            with_progress.to_bytes(512)[0..32],
            "the version-1 body must be byte-identical with and without progress"
        );
    }

    /// A checkpoint carrying no progress record must report `None` rather than
    /// a plausible cluster number. `None` is what makes the executor refuse a
    /// phase-Started resume instead of guessing where the shift stopped.
    #[test]
    fn checkpoint_without_a_progress_record_reports_none() {
        let bytes = test_checkpoint(ResizePhase::Started).to_bytes(512);

        assert_eq!(&bytes[32..44], &[0u8; 12], "no record should be written");
        assert_eq!(
            ResizeCheckpoint::from_bytes(&bytes)
                .unwrap()
                .unwrap()
                .shift_progress,
            None
        );
    }

    /// A damaged progress record is corruption, not absence. Reading it as
    /// absent would be safe; reading it as a wrong cluster number would not,
    /// so the CRC has to be enforced.
    #[test]
    fn checkpoint_rejects_a_corrupted_progress_record() {
        let good = test_checkpoint(ResizePhase::Started)
            .with_shift_progress(4321)
            .to_bytes(512);

        for offset in PROGRESS_OFFSET + 4..PROGRESS_OFFSET + PROGRESS_SIZE {
            let mut corrupt = good.clone();
            corrupt[offset] ^= 0x01;

            assert!(
                matches!(
                    ResizeCheckpoint::from_bytes(&corrupt),
                    Err(Error::CheckpointCorrupted)
                ),
                "flipping byte {} of the progress record should have been rejected",
                offset
            );
        }
    }

    /// A sector too small to hold the record still yields a usable checkpoint;
    /// the progress simply cannot be stored, and recovery treats that as
    /// unknown rather than failing outright.
    #[test]
    fn progress_record_is_omitted_when_the_sector_cannot_hold_it() {
        let bytes = test_checkpoint(ResizePhase::Started)
            .with_shift_progress(4321)
            .to_bytes(40);

        assert_eq!(bytes.len(), 40);
        assert_eq!(
            ResizeCheckpoint::from_bytes(&bytes)
                .unwrap()
                .unwrap()
                .shift_progress,
            None
        );
    }

    /// A temp-file-backed device of `sectors` zeroed 512-byte sectors.
    fn temp_device(sectors: u64) -> (NamedTempFile, Device) {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; sectors as usize * 512]).unwrap();
        let device = Device::open(file.path()).unwrap();
        (file, device)
    }

    /// The checkpoint lives in the device's last sector - deliberately, so
    /// forward-shifted cluster data starting at `old_total_sectors` cannot
    /// overwrite it. Writing it anywhere else would put it in the path of the
    /// data shift.
    #[test]
    fn checkpoint_round_trips_through_the_devices_last_sector() {
        let (_file, device) = temp_device(4000);
        // Filesystem still describes the pre-grow size, so the checkpoint
        // sector sits in the newly added space.
        let boot = test_boot_sector(2000, 100);
        let checkpoint = test_checkpoint(ResizePhase::DataCopied);

        assert!(read_checkpoint(&device, &boot).unwrap().is_none());

        write_checkpoint(&device, &checkpoint).unwrap();

        let last = device.total_sectors() - 1;
        assert_eq!(
            &device.read_sector(last).unwrap()[0..8],
            CHECKPOINT_MAGIC,
            "checkpoint must be written to the last sector"
        );

        assert_checkpoint_eq(
            &read_checkpoint(&device, &boot).unwrap().unwrap(),
            &checkpoint,
        );

        clear_checkpoint(&device).unwrap();
        assert!(read_checkpoint(&device, &boot).unwrap().is_none());
    }

    /// Once the filesystem covers the whole device there is no room for a
    /// checkpoint, so recovery must report none rather than reading whatever
    /// user data occupies the last sector.
    #[test]
    fn checkpoint_is_not_read_once_the_filesystem_fills_the_device() {
        let (_file, device) = temp_device(2000);

        // Plant a valid checkpoint in the last sector.
        write_checkpoint(&device, &test_checkpoint(ResizePhase::Started)).unwrap();

        let boot = test_boot_sector(2000, 100);
        assert!(read_checkpoint(&device, &boot).unwrap().is_none());
    }

    /// An invalidated boot sector means the resize was interrupted inside the
    /// danger zone. Recovery is only possible with a checkpoint, so its absence
    /// must surface as `InvalidatedFilesystem` rather than as "nothing to do".
    #[test]
    fn incomplete_resize_needs_a_checkpoint_when_the_signature_is_invalid() {
        let (_file, device) = temp_device(4000);
        let mut boot = test_boot_sector(2000, 100);
        boot.invalidate_signature();

        assert!(matches!(
            check_for_incomplete_resize(&device, &boot),
            Err(Error::InvalidatedFilesystem)
        ));

        let checkpoint = test_checkpoint(ResizePhase::DataCopied);
        write_checkpoint(&device, &checkpoint).unwrap();

        assert_checkpoint_eq(
            &check_for_incomplete_resize(&device, &boot)
                .unwrap()
                .unwrap(),
            &checkpoint,
        );
    }

    /// A valid signature with a checkpoint present is the phase-0 crash case:
    /// the data shift may have been interrupted before anything was
    /// invalidated, and recovery must still find the record.
    #[test]
    fn incomplete_resize_finds_a_checkpoint_behind_a_valid_signature() {
        let (_file, device) = temp_device(4000);
        let boot = test_boot_sector(2000, 100);

        assert!(check_for_incomplete_resize(&device, &boot)
            .unwrap()
            .is_none());

        let checkpoint = test_checkpoint(ResizePhase::Started);
        write_checkpoint(&device, &checkpoint).unwrap();

        assert_checkpoint_eq(
            &check_for_incomplete_resize(&device, &boot)
                .unwrap()
                .unwrap(),
            &checkpoint,
        );
    }

    #[test]
    fn test_resize_options() {
        let opts = ResizeOptions::new("/dev/sda1").dry_run(true).verbose(false);

        assert_eq!(opts.device_path(), std::path::Path::new("/dev/sda1"));
        assert!(opts.is_dry_run());
        assert!(!opts.is_verbose());
    }

    #[test]
    fn test_resize_result() {
        let calc = SizeCalculation {
            old_total_sectors: 1000000,
            new_total_sectors: 2000000,
            old_fat_size: 1000,
            new_fat_size: 2000,
            new_data_clusters: 200000,
            new_free_clusters: 100000,
            fat_needs_growth: true,
            fat_growth_sectors: 1000,
            first_affected_cluster: 2,
            last_affected_cluster: 10,
        };

        let result = ResizeResult {
            old_size_bytes: 512000000,
            new_size_bytes: 1024000000,
            fat_grew: true,
            clusters_relocated: 5,
            calculation: calc,
            operations: vec!["test".to_string()],
        };

        assert_eq!(result.old_size_bytes, 512000000);
        assert!(result.fat_grew);
        assert_eq!(result.clusters_relocated, 5);
    }
}
