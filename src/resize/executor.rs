use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::{
    boot_sectors_match, read_backup_boot_sector, read_boot_sector, read_boot_sector_for_recovery,
    read_fat_table, read_fsinfo, write_backup_boot_sector, write_boot_sector, write_fsinfo,
    BootSector, FSInfo,
};
use crate::resize::calculator::{calculate_new_size, SizeCalculation};
use crate::resize::relocator::{execute_relocation, plan_relocation, verify_relocation};
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
        }
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

        data
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
        let phase = ResizePhase::from_u8(data[9]).ok_or(Error::CheckpointCorrupted)?;

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
    /// Path to the device or image file
    pub device_path: String,
    /// Dry run mode - don't actually make changes
    pub dry_run: bool,
    /// Verbose output
    pub verbose: bool,
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
    check_not_mounted(&options.device_path)?;
    operations.push("Verified device is not mounted".to_string());

    // Open device
    let mut device = if options.dry_run {
        Device::open_readonly(&options.device_path)?
    } else {
        Device::open(&options.device_path)?
    };
    operations.push(format!("Opened device: {}", options.device_path));

    // Read boot sector - use recovery mode to allow invalidated signature
    // from an interrupted resize operation
    // This also bootstraps the device sector size from the boot sector
    let mut boot = read_boot_sector_for_recovery(&mut device)?;
    operations.push(format!(
        "Read boot sector ({}-byte sectors)",
        boot.bytes_per_sector()
    ));

    // Check for incomplete resize operation
    let incomplete_resize = if !options.dry_run {
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
        SizeCalculation {
            old_total_sectors: checkpoint.old_total_sectors,
            new_total_sectors: checkpoint.new_total_sectors,
            old_fat_size: checkpoint.old_fat_size,
            new_fat_size: checkpoint.new_fat_size,
            new_data_clusters: calculate_data_clusters_from_params(
                checkpoint.new_total_sectors,
                boot.reserved_sectors(),
                boot.num_fats(),
                checkpoint.new_fat_size,
                boot.sectors_per_cluster(),
            ),
            new_free_clusters: 0, // Will be recalculated
            fat_needs_growth: checkpoint.new_fat_size > checkpoint.old_fat_size,
            fat_growth_sectors: checkpoint
                .new_fat_size
                .saturating_sub(checkpoint.old_fat_size),
            first_affected_cluster: 2,
            // Note: formula matches calculator.rs: last = first + affected_clusters - 1
            last_affected_cluster: 2
                + (checkpoint
                    .new_fat_size
                    .saturating_sub(checkpoint.old_fat_size)
                    * boot.num_fats() as u32)
                    .div_ceil(boot.sectors_per_cluster() as u32)
                - 1,
        }
    } else {
        calculate_new_size(&boot, device_sectors)?
    };

    operations.push(format!(
        "Calculated resize: {} -> {} sectors",
        calculation.old_total_sectors, calculation.new_total_sectors
    ));

    if options.verbose {
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

    let old_size_bytes = calculation.old_total_sectors as u64 * boot.bytes_per_sector() as u64;
    let new_size_bytes = calculation.new_total_sectors as u64 * boot.bytes_per_sector() as u64;

    // Read FAT table
    let mut fat = read_fat_table(&device, &boot, 0)?;
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

        // Plan cluster data shift
        let plan = plan_relocation(
            &device,
            &boot,
            &fat,
            calculation.first_affected_cluster,
            calculation.last_affected_cluster,
            calculation.new_data_clusters,
        )?;

        if !plan.is_empty() {
            operations.push(format!(
                "Planned data shift for {} clusters ({} bytes)",
                plan.cluster_count(),
                plan.total_bytes
            ));

            if options.verbose {
                eprintln!("\nData shift plan (cluster numbers unchanged, sectors shift forward):");
                eprintln!("  {} clusters will be moved", plan.moves.len());
            }

            if !options.dry_run {
                // === PHASE 0: Data shift (safe - source preserved) ===
                if starting_phase == ResizePhase::Started {
                    // Write initial checkpoint
                    let checkpoint = ResizeCheckpoint::new(
                        ResizePhase::Started,
                        calculation.old_total_sectors,
                        calculation.new_total_sectors,
                        calculation.old_fat_size,
                        calculation.new_fat_size,
                    );
                    write_checkpoint(&device, &checkpoint)?;
                    operations.push("Wrote checkpoint (phase 0: started)".to_string());

                    maybe_crash_at("after_checkpoint_start");

                    // Execute data shift
                    let _relocations = execute_relocation(
                        &device,
                        &boot,
                        &mut fat,
                        &plan,
                        calculation.new_fat_size,
                        calculation.new_data_clusters,
                        options.verbose,
                    )?;
                    clusters_relocated = plan.cluster_count();
                    operations.push(format!("Shifted {} clusters forward", clusters_relocated));

                    verify_relocation(
                        &fat,
                        calculation.first_affected_cluster,
                        calculation.last_affected_cluster,
                    )?;
                    operations.push("Verified data shift".to_string());

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

    if !options.dry_run {
        // === PHASE 2: Metadata update (restore boot sector) ===

        // Update boot sector with new values and restore signature
        boot.set_total_sectors_32(calculation.new_total_sectors);
        boot.set_fat_size_32(calculation.new_fat_size);
        boot.restore_signature(); // Restore 0xAA55 signature

        write_boot_sector(&device, &boot)?;
        operations.push("Updated boot sector (signature restored)".to_string());

        // Update backup boot sector
        write_backup_boot_sector(&device, &boot, backup_sector)?;
        operations.push("Updated backup boot sector".to_string());

        // Update FSInfo with new free cluster count
        let old_free = fsinfo.free_count();
        let old_data_clusters = (calculation.old_total_sectors
            - boot.reserved_sectors() as u32
            - (boot.num_fats() as u32 * calculation.old_fat_size))
            / boot.sectors_per_cluster() as u32;
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

/// Helper to calculate data clusters from parameters
fn calculate_data_clusters_from_params(
    total_sectors: u32,
    reserved_sectors: u16,
    num_fats: u8,
    fat_size: u32,
    sectors_per_cluster: u8,
) -> u32 {
    let data_sectors = total_sectors
        .saturating_sub(reserved_sectors as u32)
        .saturating_sub(num_fats as u32 * fat_size);
    data_sectors / sectors_per_cluster as u32
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
pub fn get_fs_info(device_path: &str) -> Result<FSInfoReport> {
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
        device_path: device_path.to_string(),
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
    pub device_path: String,
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
        writeln!(f, "Device: {}", self.device_path)?;
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

    #[test]
    fn test_resize_options() {
        let opts = ResizeOptions {
            device_path: "/dev/sda1".to_string(),
            dry_run: true,
            verbose: false,
        };

        assert_eq!(opts.device_path, "/dev/sda1");
        assert!(opts.dry_run);
        assert!(!opts.verbose);
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
