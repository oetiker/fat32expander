use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::{Duration, UNIX_EPOCH};

use fat32expander::{
    get_fs_info, resize_fat32_with_events, Error, FSInfo, FSInfoReport, ResizeEvent, ResizeOptions,
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

        /// Proceed even if the backup boot sector disagrees with the primary
        ///
        /// This trusts the primary boot sector and rewrites the backup from
        /// it. If the primary is the damaged copy, that discards the only
        /// good record of the original layout.
        #[arg(long)]
        ignore_backup_mismatch: bool,
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
    pub const DEVICE_TOO_SMALL: u8 = 7;
    pub const BACKUP_MISMATCH: u8 = 8;
    pub const NEEDS_RECOVERY: u8 = 9;
    pub const IO_FAILURE: u8 = 10;
    pub const MOUNT_STATE_UNKNOWN: u8 = 12;
    // 6 was ALREADY_MAX_SIZE and was never returned: "nothing to do" exits
    // 0 ([F-R31]), and [F-05] removed the last code path that could have
    // produced it. Not reused, for the same reason as 11 below.
    // 11 was ROOT_REQUIRED, removed with the privilege gate in [F-R46]. A
    // caller who cannot write the device now gets DEVICE_UNOPENABLE, which is
    // what the kernel actually reported. The number is not reused: a script
    // branching on 11 should find nothing rather than a new meaning.
}

/// Map a failure to its exit code. Anything that is not one of our own errors
/// -- a context-only `anyhow` error, say -- is an internal failure.
fn exit_code_for(err: &anyhow::Error) -> u8 {
    let Some(e) = err.downcast_ref::<Error>() else {
        return exit::INTERNAL;
    };

    match e {
        Error::DeviceOpen { .. } => exit::DEVICE_UNOPENABLE,
        Error::DeviceMounted(..) | Error::DeviceBusy(_) => exit::DEVICE_MOUNTED,
        // The mount check could not look; no device was touched ([F-06]).
        Error::MountTableUnreadable { .. } => exit::MOUNT_STATE_UNKNOWN,

        Error::InvalidFAT32(_)
        | Error::BootSectorValidation(_)
        | Error::FSInfoValidation(_)
        | Error::UnsupportedSectorSize(_) => exit::NOT_USABLE_FAT32,

        // Nothing to do is success; `main` prints why.
        Error::AlreadyMaxSize => exit::OK,
        Error::DeviceTooSmall { .. } => exit::DEVICE_TOO_SMALL,
        Error::BackupMismatch => exit::BACKUP_MISMATCH,

        // Everything that means "this filesystem is mid-resize": either we can
        // still recover it, or we know we cannot and the user must intervene.
        Error::BootSectorInvalidated
        | Error::InvalidatedFilesystem
        | Error::CheckpointCorrupted
        | Error::ResizeSizeMismatch(_)
        | Error::CheckpointMismatch { .. }
        | Error::CheckpointVersionUnsupported(_)
        | Error::CheckpointVolumeMismatch { .. }
        | Error::CheckpointGeometryUnusable { .. }
        | Error::CheckpointOutsideDevice { .. }
        | Error::ShiftProgressMissing => exit::NEEDS_RECOVERY,

        Error::DeviceIo { .. }
        | Error::DeviceSync { .. }
        | Error::Io(_)
        | Error::SectorLengthMismatch { .. } => exit::IO_FAILURE,

        // An out-of-range sector means our own arithmetic was wrong, not that
        // the device misbehaved ([F-R28]).
        Error::SectorOutOfBounds { .. } => exit::INTERNAL,

        Error::Calculation(_) | Error::Relocation(_) => exit::INTERNAL,

        // `Error` is `#[non_exhaustive]` ([F-26]). A variant added without an
        // arm here fails `every_variant_is_listed` in `src/error.rs` first.
        _ => exit::INTERNAL,
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

/// Print what the library reports while a resize runs ([F-08]).
///
/// The rule for the streams: what happens during the run goes to stderr, as
/// it happens; the report of the finished run goes to stdout. Steps are not
/// printed here -- they are the "Operations performed" list of that report.
fn print_event(event: &ResizeEvent) {
    match event {
        ResizeEvent::Resuming { phase, dry_run } => {
            if *dry_run {
                eprintln!("Dry run: an interrupted resize would be resumed from phase {phase}");
            } else {
                eprintln!("Resuming interrupted resize from phase {phase}...");
            }
        }
        ResizeEvent::StaleCheckpointIgnored { sector } => {
            eprintln!(
                "Ignoring a resize checkpoint at sector {sector}: it was left by another filesystem"
            );
        }
        ResizeEvent::Layout {
            old_total_sectors,
            old_fat_size,
            old_data_clusters,
            new_total_sectors,
            new_fat_size,
            new_data_clusters,
            fat_needs_growth,
        } => {
            eprintln!("Current filesystem:");
            eprintln!("  Total sectors: {old_total_sectors}");
            eprintln!("  FAT size: {old_fat_size} sectors");
            match old_data_clusters {
                Some(clusters) => eprintln!("  Data clusters: {clusters}"),
                None => eprintln!("  Data clusters: <layout does not fit in the volume>"),
            }
            eprintln!();
            eprintln!("After resize:");
            eprintln!("  Total sectors: {new_total_sectors}");
            eprintln!("  FAT size: {new_fat_size} sectors");
            eprintln!("  Data clusters: {new_data_clusters}");
            eprintln!("  FAT needs growth: {fat_needs_growth}");
        }
        ResizeEvent::ShiftPlanned { clusters } => {
            eprintln!("\nData shift plan (cluster numbers unchanged, sectors shift forward):");
            eprintln!("  {clusters} clusters will be moved");
        }
        ResizeEvent::ShiftStarting {
            remaining,
            total,
            old_first_data_sector,
            new_first_data_sector,
            resumed_at,
        } => {
            eprintln!("Shifting data forward: {remaining} of {total} clusters need movement");
            eprintln!("  Old first data sector: {old_first_data_sector}");
            eprintln!("  New first data sector: {new_first_data_sector}");
            if let Some(cluster) = resumed_at {
                eprintln!(
                    "  Resuming at cluster {cluster} ({} already shifted)",
                    total - remaining
                );
            }
        }
        // The first ten moves, every hundredth, and the last.
        ResizeEvent::ClusterMoved {
            cluster,
            from_sector,
            to_sector,
            done,
            of,
        } if *done <= 10 || (done - 1) % 100 == 0 || done == of => {
            eprintln!(
                "Moving cluster {cluster} from sector {from_sector} to sector {to_sector} ({done}/{of})"
            );
        }
        _ => {}
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
            ignore_backup_mismatch,
        } => {
            // [F-R46]: there is deliberately no `geteuid() == 0` gate here.
            // The kernel is the authority on write access, and euid is a bad
            // proxy for it -- a user resizing a file image they own, a member
            // of group `disk`, or a caller holding CAP_SYS_ADMIN without uid 0
            // are all legitimate and were all refused by the old check. The
            // open fails with EACCES if access is genuinely denied, and that
            // error names both the device and the errno ([F-R29], [F-R30]).
            //
            // The gate's own escape hatch is what made this worse: `--force`
            // switched it off, so the flag users reached for to get past a
            // privilege guess also switched off the backup-mismatch check,
            // which is a data-loss decision. One flag, one meaning.

            // [F-05]: whether and how to resize is the library's decision
            // alone. This used to decide "nothing to do" and "backup mismatch"
            // here too, from a second open of the device, and got the first
            // one wrong: a device smaller than its filesystem was reported as
            // "already at maximum size" with exit 0 instead of exit 7. The
            // filesystem is now read only for display. A filesystem that
            // cannot be read at all still stops here, with the same error the
            // library would raise, so the message keeps naming the read.
            match get_fs_info(&device) {
                Ok(info) => {
                    if verbose {
                        println!("Current filesystem state:");
                        println!("{}", render_fs_info(&info));
                        println!();
                    }
                }
                // A zeroed boot signature is the marker this tool writes
                // across the unsafe window, so it means "our own resize was
                // interrupted" -- not "this filesystem is damaged".
                //
                // The gate is deliberately this narrow. A boot sector
                // damaged beyond its signature fails earlier (on its
                // declared sector size) and never arrives here, which is
                // correct: this tool guards against its own process dying,
                // not against arbitrary corruption.
                Err(Error::BootSectorInvalidated) => {
                    eprintln!("Warning: Boot sector appears to be invalidated.");
                    eprintln!("         This may indicate an interrupted resize operation.");
                    eprintln!("         Attempting recovery...");
                    eprintln!();
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to read filesystem info from {}", device)
                    });
                }
            }

            if dry_run {
                println!("DRY RUN MODE - No changes will be made");
                println!();
            }

            // Perform the resize
            let options = ResizeOptions::new(&device)
                .dry_run(dry_run)
                .verbose(verbose)
                .ignore_backup_mismatch(ignore_backup_mismatch);

            // [F-07]: the steps are kept as they arrive, so a resize that
            // fails still shows how far it got -- whether the boot sector was
            // already invalidated, for one. "Nothing to do" is not a failure.
            let mut steps = Vec::new();
            let outcome = resize_fat32_with_events(options, &mut |event| {
                if let ResizeEvent::Step(step) = event {
                    steps.push(step.clone());
                }
                print_event(event);
            });
            if outcome.is_err()
                && !matches!(outcome, Err(Error::AlreadyMaxSize))
                && !steps.is_empty()
            {
                eprintln!("Operations performed before the failure:");
                for step in &steps {
                    eprintln!("  - {}", step);
                }
                eprintln!();
            }
            let result =
                outcome.with_context(|| format!("Failed to resize filesystem on {}", device))?;

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
            println!("  Device: {}", device);
            println!(
                "  Old size: {:.2} MB ({} bytes)",
                result.old_size_bytes as f64 / (1024.0 * 1024.0),
                result.old_size_bytes
            );
            println!(
                "  New size: {:.2} MB ({} bytes)",
                result.new_size_bytes as f64 / (1024.0 * 1024.0),
                result.new_size_bytes
            );
            let increase = result.new_size_bytes.saturating_sub(result.old_size_bytes);
            println!(
                "  Size increase: {:.2} MB ({} bytes)",
                increase as f64 / (1024.0 * 1024.0),
                increase
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

#[cfg(test)]
mod tests {
    use super::*;

    fn code(e: Error) -> u8 {
        exit_code_for(&anyhow::Error::new(e))
    }

    /// [F-06]: a resume the tool refuses is a filesystem that needs recovery,
    /// not an internal failure.
    #[test]
    fn recovery_refusals_exit_with_needs_recovery() {
        assert_eq!(code(Error::ShiftProgressMissing), exit::NEEDS_RECOVERY);
        assert_eq!(
            code(Error::CheckpointGeometryUnusable {
                new_total_sectors: 1000,
                new_fat_size: 900,
            }),
            exit::NEEDS_RECOVERY
        );
    }

    /// [F-16]: a record from a newer build, or of another volume, is a
    /// refused resume like any other.
    #[test]
    fn checkpoint_format_refusals_exit_with_needs_recovery() {
        assert_eq!(
            code(Error::CheckpointVersionUnsupported(3)),
            exit::NEEDS_RECOVERY
        );
        assert_eq!(
            code(Error::CheckpointVolumeMismatch { record: 1, boot: 2 }),
            exit::NEEDS_RECOVERY
        );
    }

    /// Safety net for [F-11]: however the mount guard learns that the device
    /// is in use, the refusal keeps its exit code.
    #[test]
    fn a_mounted_device_exits_4() {
        let e = Error::DeviceMounted("/dev/sdx1".to_string(), "/mnt".to_string());
        assert_eq!(code(e), exit::DEVICE_MOUNTED);
        assert_eq!(exit::DEVICE_MOUNTED, 4);
    }

    /// [F-11]: the kernel refusing an exclusive open is a device in use.
    #[test]
    fn a_busy_device_exits_like_a_mounted_one() {
        assert_eq!(
            code(Error::DeviceBusy("/dev/sdx1".to_string())),
            exit::DEVICE_MOUNTED
        );
    }

    /// [F-06]: failing to read the mount table touched no device, so it is
    /// not a device I/O failure.
    #[test]
    fn an_unreadable_mount_table_has_its_own_exit_code() {
        let e = Error::MountTableUnreadable {
            device: "/dev/sdx1".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(code(e), exit::MOUNT_STATE_UNKNOWN);
        assert_eq!(exit::MOUNT_STATE_UNKNOWN, 12);
    }

    /// Safety net for [F-26]: every error variant keeps its exit code. The
    /// match in `exit_code_for` is exhaustive today, so a new variant cannot
    /// compile without a code; marking `Error` `#[non_exhaustive]` would need a
    /// wildcard arm there, and this table is what notices a variant falling
    /// into it. Literal numbers, not the `exit` constants: the codes are an ABI.
    #[test]
    fn every_error_variant_keeps_its_exit_code() {
        let io = || std::io::Error::from(std::io::ErrorKind::Other);
        let s = || "x".to_string();
        let table: Vec<(Error, u8)> = vec![
            (
                Error::DeviceOpen {
                    path: s(),
                    source: io(),
                },
                3,
            ),
            (Error::DeviceMounted(s(), s()), 4),
            (Error::DeviceBusy(s()), 4),
            (Error::InvalidFAT32(s()), 5),
            (Error::BootSectorValidation(s()), 5),
            (Error::BootSectorInvalidated, 9),
            (Error::FSInfoValidation(s()), 5),
            (Error::BackupMismatch, 8),
            (
                Error::DeviceTooSmall {
                    current: 1,
                    minimum: 2,
                },
                7,
            ),
            (Error::AlreadyMaxSize, 0),
            (Error::Io(io()), 10),
            (
                Error::DeviceIo {
                    op: "read",
                    path: s(),
                    offset: 0,
                    sector: 0,
                    len: 1,
                    source: io(),
                },
                10,
            ),
            (
                Error::DeviceSync {
                    path: s(),
                    source: io(),
                },
                10,
            ),
            (
                Error::SectorLengthMismatch {
                    path: s(),
                    expected: 512,
                    actual: 1,
                },
                10,
            ),
            (
                Error::SectorOutOfBounds {
                    path: s(),
                    sector: 9,
                    count: 1,
                    total_sectors: 8,
                },
                1,
            ),
            (Error::Calculation(s()), 1),
            (Error::Relocation(s()), 1),
            (Error::UnsupportedSectorSize(3), 5),
            (Error::CheckpointCorrupted, 9),
            (Error::InvalidatedFilesystem, 9),
            (Error::ResizeSizeMismatch(1), 9),
            (
                Error::CheckpointMismatch {
                    old_total_sectors: 1,
                    new_total_sectors: 2,
                    old_fat_size: 1,
                    new_fat_size: 2,
                    boot_total_sectors: 3,
                    boot_fat_size: 3,
                },
                9,
            ),
            (Error::CheckpointVersionUnsupported(3), 9),
            (Error::CheckpointVolumeMismatch { record: 1, boot: 2 }, 9),
            (
                Error::CheckpointOutsideDevice {
                    sector: 9,
                    device_sectors: 8,
                },
                9,
            ),
            (
                Error::CheckpointGeometryUnusable {
                    new_total_sectors: 1,
                    new_fat_size: 1,
                },
                9,
            ),
            (Error::ShiftProgressMissing, 9),
            (
                Error::MountTableUnreadable {
                    device: s(),
                    source: io(),
                },
                12,
            ),
        ];

        for (err, expected) in table {
            let name = format!("{err:?}");
            assert_eq!(code(err), expected, "exit code of {name}");
        }
        // Not one of our errors at all.
        assert_eq!(exit_code_for(&anyhow::anyhow!("context only")), 1);
    }
}
