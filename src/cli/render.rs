//! How the command shows a filesystem report and a resize to a person.

use fat32expander::{FSInfo, FSInfoReport, ResizeEvent, ResizeResult};

/// `bytes` in mebibytes, for display next to the exact byte count.
fn mib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Render a filesystem report for a human reader.
///
/// [F-R42] moved this out of the library. `FSInfoReport` is data; section
/// headers, indentation and "Yes"/"NO" strings are terminal presentation, and a
/// library type carrying a `Display` impl full of them put rendering on the
/// wrong side of the boundary. Scripts consuming the library now get the fields
/// without having to parse prose.
pub fn render_fs_info(info: &FSInfoReport) -> String {
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
        "  Current size: {} bytes ({:.2} MiB)\n",
        info.current_size_bytes(),
        mib(info.current_size_bytes())
    ));
    out.push_str(&format!("  Device sectors: {}\n", info.device_sectors()));
    out.push_str(&format!(
        "  Can grow: {}\n",
        if info.can_grow() { "Yes" } else { "No" }
    ));
    if let Some(max_size) = info.max_new_size_bytes() {
        out.push_str(&format!(
            "  Max new size: {} bytes ({:.2} MiB)\n",
            max_size,
            mib(max_size)
        ));
    }

    out
}

/// Print what the library reports while a resize runs ([F-08]).
///
/// The rule for the streams: what happens during the run goes to stderr, as
/// it happens; the report of the finished run goes to stdout. Steps are not
/// printed here -- they are the "Operations performed" list of that report.
pub fn print_event(event: &ResizeEvent) {
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

/// Print the report of a finished resize on stdout.
pub fn print_resize_report(device: &str, result: &ResizeResult, dry_run: bool) {
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
        "  Old size: {:.2} MiB ({} bytes)",
        mib(result.old_size_bytes),
        result.old_size_bytes
    );
    println!(
        "  New size: {:.2} MiB ({} bytes)",
        mib(result.new_size_bytes),
        result.new_size_bytes
    );
    let increase = result.new_size_bytes.saturating_sub(result.old_size_bytes);
    println!(
        "  Size increase: {:.2} MiB ({} bytes)",
        mib(increase),
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
