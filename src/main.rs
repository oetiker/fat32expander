mod cli;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use cli::exit::{self, exit_code_for};
use cli::render::render_fs_info;
use cli::version::version_long;
use fat32expander::{get_fs_info, Error};

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
            ignore_backup_mismatch,
        } => cli::resize::resize(&device, dry_run, verbose, ignore_backup_mismatch)?,
    }

    Ok(())
}
