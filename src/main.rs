use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::time::{Duration, UNIX_EPOCH};

use fat32expander::{check_root, get_fs_info, resize_fat32, ResizeOptions};

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

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { device } => {
            let info = get_fs_info(&device)
                .with_context(|| format!("Failed to read filesystem info from {}", device))?;
            println!("{}", info);
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
                    anyhow::bail!("Run as root or use --force to continue anyway");
                }
            }

            // Try to show current state - may fail if boot sector is invalidated from crash
            let info_result = get_fs_info(&device);
            let (show_pre_info, current_size, new_size) = match info_result {
                Ok(info) => {
                    if verbose {
                        println!("Current filesystem state:");
                        println!("{}", info);
                        println!();
                    }

                    // Check if resize is possible
                    if !info.can_grow {
                        anyhow::bail!(
                            "Filesystem is already at maximum size for the device ({} bytes)",
                            info.current_size_bytes
                        );
                    }

                    if !info.backup_matches && !force {
                        eprintln!(
                            "Warning: Backup boot sector does not match primary boot sector."
                        );
                        eprintln!("         This could indicate filesystem corruption.");
                        anyhow::bail!("Use --force to proceed anyway");
                    }

                    let new_size = info.max_new_size_bytes.unwrap_or(info.current_size_bytes);
                    (true, info.current_size_bytes, new_size)
                }
                Err(e) => {
                    // Check if this might be an invalidated boot sector from a crash
                    let err_msg = format!("{:?}", e);
                    if err_msg.contains("Invalid boot signature: 0x0000") {
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
