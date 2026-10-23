//! The `resize` subcommand.

use anyhow::{Context, Result};
use fat32expander::{get_fs_info, resize_fat32_with_events, Error, ResizeEvent, ResizeOptions};

use super::render::{print_event, print_resize_report, render_fs_info};

/// Resize the filesystem on `device` and report on the terminal.
pub fn resize(
    device: &str,
    dry_run: bool,
    verbose: bool,
    ignore_backup_mismatch: bool,
) -> Result<()> {
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
    match get_fs_info(device) {
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
            return Err(e)
                .with_context(|| format!("Failed to read filesystem info from {}", device));
        }
    }

    // The report opens with a blank line, so the banner needs none.
    if dry_run {
        println!("DRY RUN MODE - No changes will be made");
    }

    // Perform the resize
    let options = ResizeOptions::new(device)
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
    if outcome.is_err() && !matches!(outcome, Err(Error::AlreadyMaxSize)) && !steps.is_empty() {
        eprintln!("Operations performed before the failure:");
        for step in &steps {
            eprintln!("  - {}", step);
        }
        eprintln!();
    }
    let result = outcome.with_context(|| format!("Failed to resize filesystem on {}", device))?;

    print_resize_report(device, &result, dry_run);
    Ok(())
}
