use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::{Duration, UNIX_EPOCH};

use fat32expander::{
    check_root, get_fs_info, resize_fat32, Error, FSInfo, FSInfoReport, ResizeOptions,
};

const BUILD_TIMESTAMP: u64 = const_parse_u64(env!("BUILD_TIMESTAMP"));
const GIT_HASH: &str = env!("GIT_HASH");

const fn const_parse_u64(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut result: u64 = 0;
    let mut i = 0;
    while i < bytes.len() {
        result = result * 10 + (bytes[i] - b'0') as u64;
        i += 1;
    }
    result
}

fn format_build_time() -> String {
    let dt = UNIX_EPOCH + Duration::from_secs(BUILD_TIMESTAMP);
    let secs = dt.duration_since(UNIX_EPOCH).unwrap().as_secs();
    // Simple UTC formatting: YYYY-MM-DD HH:MM:SS
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Calculate year/month/day from days since 1970-01-01
    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, hours, minutes, seconds
    )
}

fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let mut remaining = days as i64;
    let mut year = 1970i64;

    loop {
        let days_in_year = if is_leap_year(year) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        year += 1;
    }

    let days_in_months: [i64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1i64;
    for days in days_in_months {
        if remaining < days {
            break;
        }
        remaining -= days;
        month += 1;
    }

    (year as u64, month as u64, (remaining + 1) as u64)
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn version_long() -> String {
    format!(
        "{} (built {} git:{})",
        env!("CARGO_PKG_VERSION"),
        format_build_time(),
        GIT_HASH
    )
}

#[derive(Parser)]
#[command(name = "fat32expander")]
#[command(author, version, about = "Resize FAT32 filesystems in-place", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Display information about a FAT32 filesystem
    Info {
        /// Path to the device or image file
        device: String,
    },

    /// Show detailed version and build information
    Version,

    /// Resize a FAT32 filesystem to fill its partition
    Resize {
        /// Path to the device or image file
        device: String,

        /// Dry run - show what would be done without making changes
        #[arg(short = 'n', long)]
        dry_run: bool,

        /// Verbose output
        #[arg(short, long)]
        verbose: bool,

        /// Force resize even if warnings are present
        #[arg(short, long)]
        force: bool,
    },
}

/// Render a filesystem report for a human reader.
///
/// [F-R42] moved this out of the library. `FSInfoReport` is data; section
/// headers, indentation and "Yes"/"NO" strings are terminal presentation, and a
/// library type carrying a `Display` impl full of them put rendering on the
/// wrong side of the boundary. Scripts consuming the library now get the fields
/// without having to parse prose.
fn render_fs_info(info: &FSInfoReport) -> String {
    let mut out = String::new();

    out.push_str("FAT32 Filesystem Information\n");
    out.push_str("============================\n");
    out.push_str(&format!("Device: {}\n", info.device_path().display()));

    out.push_str("\nGeometry:\n");
    out.push_str(&format!(
        "  Bytes per sector: {}\n",
        info.boot().bytes_per_sector()
    ));
    out.push_str(&format!(
        "  Sectors per cluster: {}\n",
        info.boot().sectors_per_cluster()
    ));
    out.push_str(&format!(
        "  Bytes per cluster: {}\n",
        info.boot().bytes_per_sector() as u32 * info.boot().sectors_per_cluster() as u32
    ));

    out.push_str("\nLayout:\n");
    out.push_str(&format!(
        "  Reserved sectors: {}\n",
        info.boot().reserved_sectors()
    ));
    out.push_str(&format!("  Number of FATs: {}\n", info.boot().num_fats()));
    out.push_str(&format!(
        "  FAT size (sectors): {}\n",
        info.boot().fat_size()
    ));
    out.push_str(&format!(
        "  Total sectors: {}\n",
        info.boot().total_sectors()
    ));
    out.push_str(&format!("  Data clusters: {}\n", info.data_clusters()));

    out.push_str("\nSpecial sectors:\n");
    out.push_str(&format!(
        "  Root directory cluster: {}\n",
        info.boot().root_cluster()
    ));
    out.push_str(&format!(
        "  FSInfo sector: {}\n",
        info.boot().fs_info_sector()
    ));
    out.push_str(&format!(
        "  Backup boot sector: {}\n",
        info.boot().backup_boot_sector()
    ));
    out.push_str(&format!(
        "  Backup matches primary: {}\n",
        if info.backup_matches() { "Yes" } else { "NO" }
    ));

    out.push_str("\nUsage:\n");
    if info.free_clusters() == FSInfo::UNKNOWN_FREE {
        out.push_str("  Free clusters: Unknown\n");
    } else {
        let free_bytes = info.free_clusters() as u64
            * info.boot().bytes_per_sector() as u64
            * info.boot().sectors_per_cluster() as u64;
        out.push_str(&format!(
            "  Free clusters: {} ({} bytes)\n",
            info.free_clusters(),
            free_bytes
        ));
    }

    out.push_str("\nSize:\n");
    out.push_str(&format!(
        "  Current size: {} bytes ({:.2} MB)\n",
        info.current_size_bytes(),
        info.current_size_bytes() as f64 / (1024.0 * 1024.0)
    ));
    out.push_str(&format!("  Device sectors: {}\n", info.device_sectors()));
    out.push_str(&format!(
        "  Can grow: {}\n",
        if info.can_grow() { "Yes" } else { "No" }
    ));
    if let Some(max_size) = info.max_new_size_bytes() {
        out.push_str(&format!(
            "  Max new size: {} bytes ({:.2} MB)\n",
            max_size,
            max_size as f64 / (1024.0 * 1024.0)
        ));
    }

    out
}

/// Exit codes, grouped by what a caller should *do* about the failure.
///
/// Deliberately not one code per error variant: the exit code is an ABI, and
/// the error enum changes shape far too often to be one. Callers branch on
/// "can't open the device" or "needs recovery", not on which constructor ran.
///
/// 2 is left free for clap's own usage errors, and everything stays below 125,
/// where the shell's own conventions (126, 127, 128+n) begin.
mod exit {
    pub const OK: u8 = 0;
    pub const INTERNAL: u8 = 1;
    pub const DEVICE_UNOPENABLE: u8 = 3;
    pub const DEVICE_MOUNTED: u8 = 4;
    pub const NOT_USABLE_FAT32: u8 = 5;
    pub const ALREADY_MAX_SIZE: u8 = 6;
    pub const DEVICE_TOO_SMALL: u8 = 7;
    pub const BACKUP_MISMATCH: u8 = 8;
    pub const NEEDS_RECOVERY: u8 = 9;
    pub const IO_FAILURE: u8 = 10;
    pub const ROOT_REQUIRED: u8 = 11;
}

/// Map a failure to its exit code. Anything that is not one of our own errors
/// -- a context-only `anyhow` error, say -- is an internal failure.
fn exit_code_for(err: &anyhow::Error) -> u8 {
    let Some(e) = err.downcast_ref::<Error>() else {
        return exit::INTERNAL;
    };

    match e {
        Error::DeviceOpen { .. } => exit::DEVICE_UNOPENABLE,
        Error::DeviceMounted(..) => exit::DEVICE_MOUNTED,

        Error::InvalidFAT32(_)
        | Error::BootSectorValidation(_)
        | Error::FSInfoValidation(_)
        | Error::UnsupportedSectorSize(_) => exit::NOT_USABLE_FAT32,

        Error::AlreadyMaxSize => exit::ALREADY_MAX_SIZE,
        Error::DeviceTooSmall { .. } => exit::DEVICE_TOO_SMALL,
        Error::BackupMismatch => exit::BACKUP_MISMATCH,

        // Everything that means "this filesystem is mid-resize": either we can
        // still recover it, or we know we cannot and the user must intervene.
        Error::BootSectorInvalidated
        | Error::InvalidatedFilesystem
        | Error::CheckpointCorrupted
        | Error::ResizeSizeMismatch(_) => exit::NEEDS_RECOVERY,

        Error::DeviceIo { .. }
        | Error::DeviceSync { .. }
        | Error::Io(_)
        | Error::SectorLengthMismatch { .. } => exit::IO_FAILURE,

        Error::RootRequired => exit::ROOT_REQUIRED,

        // An out-of-range sector means our own arithmetic was wrong, not that
        // the device misbehaved ([F-R28]).
        Error::SectorOutOfBounds { .. } => exit::INTERNAL,

        Error::Calculation(_) | Error::Relocation(_) => exit::INTERNAL,
    }
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::from(exit::OK),
        Err(err) => {
            // "Already at maximum size" reaching here means the library found
            // nothing to do. That is a successful no-op, not a failure: the
            // tool has to be safe to re-run from a provisioning script that
            // does not track whether it already ran.
            if matches!(err.downcast_ref::<Error>(), Some(Error::AlreadyMaxSize)) {
                println!("Filesystem is already at maximum size for the device. Nothing to do.");
                return std::process::ExitCode::from(exit::OK);
            }

            // Same rendering anyhow's own `fn main() -> Result` produces, so
            // the message and its "Caused by:" chain are unchanged.
            eprintln!("Error: {:?}", err);
            std::process::ExitCode::from(exit_code_for(&err))
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { device } => {
            let info = get_fs_info(&device)
                .with_context(|| format!("Failed to read filesystem info from {}", device))?;
            println!("{}", render_fs_info(&info));
        }

        Commands::Version => {
            println!("fat32expander {}", version_long());
        }

        Commands::Resize {
            device,
            dry_run,
            verbose,
            force,
        } => {
            // Check for root privileges
            if !check_root() && !dry_run {
                eprintln!("Warning: This tool requires root privileges to modify block devices.");
                eprintln!("         Use --dry-run to preview changes without root.");
                if !force {
                    return Err(Error::RootRequired.into());
                }
            }

            // Try to show current state - may fail if boot sector is invalidated from crash
            let info_result = get_fs_info(&device);
            let (show_pre_info, current_size, new_size) = match info_result {
                Ok(info) => {
                    if verbose {
                        println!("Current filesystem state:");
                        println!("{}", render_fs_info(&info));
                        println!();
                    }

                    // Nothing to do is not a failure -- the tool must be safe to
                    // re-run from a script that does not track whether it has
                    // already run ([F-R31]).
                    if !info.can_grow() {
                        println!(
                            "Filesystem is already at maximum size for the device ({} bytes). \
                             Nothing to do.",
                            info.current_size_bytes()
                        );
                        return Ok(());
                    }

                    if !info.backup_matches() && !force {
                        eprintln!(
                            "Warning: Backup boot sector does not match primary boot sector."
                        );
                        eprintln!("         This could indicate filesystem corruption.");
                        eprintln!("         Use --force to proceed anyway.");
                        return Err(Error::BackupMismatch.into());
                    }

                    let new_size = info
                        .max_new_size_bytes()
                        .unwrap_or(info.current_size_bytes());
                    (true, info.current_size_bytes(), new_size)
                }
                Err(e) => {
                    // A zeroed boot signature is the marker this tool writes
                    // across the unsafe window, so it means "our own resize was
                    // interrupted" -- not "this filesystem is damaged".
                    //
                    // The gate is deliberately this narrow. A boot sector
                    // damaged beyond its signature fails earlier (on its
                    // declared sector size) and never arrives here, which is
                    // correct: this tool guards against its own process dying,
                    // not against arbitrary corruption, and driving the resize
                    // machinery over a filesystem it never touched would be
                    // worse than refusing.
                    if matches!(e, Error::BootSectorInvalidated) {
                        eprintln!("Warning: Boot sector appears to be invalidated.");
                        eprintln!("         This may indicate an interrupted resize operation.");
                        eprintln!("         Attempting recovery...");
                        eprintln!();
                        // We can't show info, but resize_fat32 will handle recovery
                        (false, 0, 0)
                    } else {
                        return Err(e).with_context(|| {
                            format!("Failed to read filesystem info from {}", device)
                        });
                    }
                }
            };

            // Calculate size increase (if we have the info)
            let increase = new_size.saturating_sub(current_size);

            if show_pre_info {
                println!("Resize operation:");
                println!("  Device: {}", device);
                println!(
                    "  Current size: {:.2} MB ({} bytes)",
                    current_size as f64 / (1024.0 * 1024.0),
                    current_size
                );
                println!(
                    "  New size: {:.2} MB ({} bytes)",
                    new_size as f64 / (1024.0 * 1024.0),
                    new_size
                );
                println!(
                    "  Size increase: {:.2} MB ({} bytes)",
                    increase as f64 / (1024.0 * 1024.0),
                    increase
                );
                println!();
            }

            if dry_run {
                println!("DRY RUN MODE - No changes will be made");
                println!();
            }

            // Perform the resize
            let options = ResizeOptions::new(&device)
                .dry_run(dry_run)
                .verbose(verbose);

            let result = resize_fat32(options)
                .with_context(|| format!("Failed to resize filesystem on {}", device))?;

            // Print results
            println!();
            println!(
                "Resize {}!",
                if dry_run {
                    "preview complete"
                } else {
                    "complete"
                }
            );
            println!();
            println!("Operations performed:");
            for op in &result.operations {
                println!("  - {}", op);
            }
            println!();
            println!("Summary:");
            println!(
                "  Old size: {:.2} MB",
                result.old_size_bytes as f64 / (1024.0 * 1024.0)
            );
            println!(
                "  New size: {:.2} MB",
                result.new_size_bytes as f64 / (1024.0 * 1024.0)
            );
            println!("  FAT tables grew: {}", result.fat_grew);
            if result.clusters_relocated > 0 {
                println!("  Clusters relocated: {}", result.clusters_relocated);
            }

            if !dry_run {
                println!();
                println!("The filesystem has been resized successfully.");
            }
        }
    }

    Ok(())
}
