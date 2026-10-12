//! Safety net for [F-25]: what the `fat32expander` command prints, on which
//! stream, in which format. The tests run the binary, so they hold whatever
//! module the code that prints lives in.
//!
//! The expected text is built from what the library reports for an identical
//! image, so the numbers come from the library and the format from here.
//!
//! The fixture is hand-built, so these run on every `cargo test`.

use fat32expander::{resize_fat32_with_events, ResizeEvent, ResizeOptions, ResizeResult};
use std::path::Path;
use std::process::{Command, Output};
use tempfile::NamedTempFile;

const BIN: &str = env!("CARGO_BIN_EXE_fat32expander");

const TOTAL_SECTORS: u32 = 70_000;
const DEVICE_SECTORS: usize = 90_000;
const RESERVED_SECTORS: usize = 32;
const FAT_SIZE: usize = 550;

/// Clusters 2 up to this one are in use. 250 of them: enough that the CLI's
/// sampling of cluster moves (the first ten, every hundredth, the last) has
/// something to leave out.
const LAST_USED_CLUSTER: u32 = 251;

/// A FAT32 volume of 70,000 sectors on a 90,000-sector file with clusters
/// 2..=251 in use, so a resize grows the FAT and shifts 250 clusters.
fn growable_image() -> NamedTempFile {
    let mut data = vec![0u8; DEVICE_SECTORS * 512];
    let boot = &mut data[0..512];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MSDOS5.0");
    boot[11..13].copy_from_slice(&512u16.to_le_bytes());
    boot[13] = 1; // sectors per cluster
    boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = 2; // number of FATs
    boot[21] = 0xF8;
    boot[32..36].copy_from_slice(&TOTAL_SECTORS.to_le_bytes());
    boot[36..40].copy_from_slice(&(FAT_SIZE as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    boot[48] = 1; // FSInfo sector
    boot[50] = 6; // backup boot sector
    boot[67..71].copy_from_slice(&0x1234_5678u32.to_le_bytes()); // volume id
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;
    let primary = data[0..512].to_vec();
    data[6 * 512..7 * 512].copy_from_slice(&primary);

    let fsinfo = &mut data[512..1024];
    fsinfo[0..4].copy_from_slice(b"RRaA");
    fsinfo[484..488].copy_from_slice(b"rrAa");
    fsinfo[488..492].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    fsinfo[492..496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    fsinfo[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

    for fat in 0..2 {
        let at = (RESERVED_SECTORS + fat * FAT_SIZE) * 512;
        for entry in 0..=LAST_USED_CLUSTER as usize {
            let value: u32 = if entry == 0 { 0x0FFF_FFF8 } else { 0x0FFF_FFFF };
            data[at + entry * 4..at + entry * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    let file = NamedTempFile::new_in(env!("CARGO_TARGET_TMPDIR")).expect("temp file");
    std::fs::write(file.path(), &data).expect("write image");
    file
}

fn run(args: &[&str], path: &Path) -> Output {
    Command::new(BIN)
        .args(args)
        .arg(path)
        .output()
        .expect("failed to run fat32expander")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("output must be UTF-8")
}

/// Resize an identical image through the library and keep what it reported.
/// The steps name the library's image; they are rewritten to name `device`,
/// the image the CLI is run on.
fn library_resize(device: &Path) -> (ResizeResult, Vec<ResizeEvent>) {
    let image = growable_image();
    let mut events = Vec::new();
    let mut result =
        resize_fat32_with_events(ResizeOptions::new(image.path()).verbose(true), &mut |e| {
            events.push(e.clone())
        })
        .expect("library resize must succeed");
    let (ours, theirs) = (
        image.path().display().to_string(),
        device.display().to_string(),
    );
    for op in &mut result.operations {
        *op = op.replace(&ours, &theirs);
    }
    (result, events)
}

/// The report a finished, non-dry-run resize of `device` prints on stdout.
fn expected_report(device: &Path, result: &ResizeResult) -> String {
    let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
    let increase = result.new_size_bytes - result.old_size_bytes;
    let mut s = String::from("\nResize complete!\n\nOperations performed:\n");
    for op in &result.operations {
        s += &format!("  - {op}\n");
    }
    s += "\nSummary:\n";
    s += &format!("  Device: {}\n", device.display());
    s += &format!(
        "  Old size: {:.2} MiB ({} bytes)\n",
        mib(result.old_size_bytes),
        result.old_size_bytes
    );
    s += &format!(
        "  New size: {:.2} MiB ({} bytes)\n",
        mib(result.new_size_bytes),
        result.new_size_bytes
    );
    s += &format!(
        "  Size increase: {:.2} MiB ({} bytes)\n",
        mib(increase),
        increase
    );
    s += &format!("  FAT tables grew: {}\n", result.fat_grew);
    s += &format!("  Clusters relocated: {}\n", result.clusters_relocated);
    s += "\nThe filesystem has been resized successfully.\n";
    s
}

/// What a verbose resize prints on stderr for these events.
fn expected_verbose_stderr(events: &[ResizeEvent]) -> String {
    let mut s = String::new();
    for event in events {
        match event {
            ResizeEvent::Layout {
                old_total_sectors,
                old_fat_size,
                old_data_clusters,
                new_total_sectors,
                new_fat_size,
                new_data_clusters,
                fat_needs_growth,
            } => {
                s += "Current filesystem:\n";
                s += &format!("  Total sectors: {old_total_sectors}\n");
                s += &format!("  FAT size: {old_fat_size} sectors\n");
                s += &format!(
                    "  Data clusters: {}\n",
                    old_data_clusters.expect("the fixture's old layout fits")
                );
                s += "\nAfter resize:\n";
                s += &format!("  Total sectors: {new_total_sectors}\n");
                s += &format!("  FAT size: {new_fat_size} sectors\n");
                s += &format!("  Data clusters: {new_data_clusters}\n");
                s += &format!("  FAT needs growth: {fat_needs_growth}\n");
            }
            ResizeEvent::ShiftPlanned { clusters } => {
                s += "\nData shift plan (cluster numbers unchanged, sectors shift forward):\n";
                s += &format!("  {clusters} clusters will be moved\n");
            }
            ResizeEvent::ShiftStarting {
                remaining,
                total,
                old_first_data_sector,
                new_first_data_sector,
                resumed_at,
            } => {
                assert!(resumed_at.is_none(), "the fixture is a fresh resize");
                s += &format!(
                    "Shifting data forward: {remaining} of {total} clusters need movement\n"
                );
                s += &format!("  Old first data sector: {old_first_data_sector}\n");
                s += &format!("  New first data sector: {new_first_data_sector}\n");
            }
            ResizeEvent::ClusterMoved {
                cluster,
                from_sector,
                to_sector,
                done,
                of,
            } if [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 101, 201, 250].contains(done) => {
                assert_eq!(*of, 250);
                s += &format!(
                    "Moving cluster {cluster} from sector {from_sector} to sector {to_sector} ({done}/{of})\n"
                );
            }
            _ => {}
        }
    }
    s
}

#[test]
fn a_resize_prints_its_report_on_stdout_and_nothing_on_stderr() {
    let image = growable_image();
    let (result, _) = library_resize(image.path());
    assert_eq!(result.clusters_relocated, 250, "fixture sanity");

    let out = run(&["resize"], image.path());

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", text(&out.stderr));
    assert_eq!(text(&out.stdout), expected_report(image.path(), &result));
    assert_eq!(
        text(&out.stderr),
        "",
        "a quiet resize writes nothing to stderr"
    );
}

#[test]
fn a_verbose_resize_shows_the_filesystem_on_stdout_and_its_progress_on_stderr() {
    let image = growable_image();
    let (result, events) = library_resize(image.path());
    let info = run(&["info"], image.path());
    assert_eq!(
        info.status.code(),
        Some(0),
        "stderr:\n{}",
        text(&info.stderr)
    );

    let out = run(&["resize", "--verbose"], image.path());

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", text(&out.stderr));
    assert_eq!(
        text(&out.stdout),
        format!(
            "Current filesystem state:\n{}\n{}",
            text(&info.stdout),
            expected_report(image.path(), &result)
        )
    );
    assert_eq!(text(&out.stderr), expected_verbose_stderr(&events));
}

#[test]
fn version_prints_the_build_time_as_a_utc_date_and_time() {
    let out = Command::new(BIN)
        .arg("version")
        .output()
        .expect("failed to run fat32expander");
    let stdout = text(&out.stdout);

    let prefix = format!("fat32expander {} (built ", env!("CARGO_PKG_VERSION"));
    let rest = stdout
        .strip_prefix(&prefix)
        .unwrap_or_else(|| panic!("expected {prefix:?} first, got:\n{stdout}"));
    // "YYYY-MM-DD HH:MM:SS UTC git:"
    let stamp = &rest[..28];
    let digits: Vec<u32> = [
        &stamp[0..4],
        &stamp[5..7],
        &stamp[8..10],
        &stamp[11..13],
        &stamp[14..16],
        &stamp[17..19],
    ]
    .iter()
    .map(|d| {
        d.parse()
            .unwrap_or_else(|_| panic!("not a number in {stamp:?}"))
    })
    .collect();
    assert_eq!(
        (
            &stamp[4..5],
            &stamp[7..8],
            &stamp[10..11],
            &stamp[13..14],
            &stamp[16..17],
            &stamp[19..28]
        ),
        ("-", "-", " ", ":", ":", " UTC git:"),
        "got {stamp:?}"
    );
    let [year, month, day, hour, minute, second] = digits[..] else {
        unreachable!()
    };
    assert!((2025..2200).contains(&year), "year {year}");
    assert!((1..=12).contains(&month), "month {month}");
    assert!((1..=31).contains(&day), "day {day}");
    assert!(hour < 24 && minute < 60 && second < 60, "time in {stamp:?}");
    assert!(stdout.ends_with(")\n"), "got:\n{stdout}");
}

#[test]
fn info_prints_the_current_and_the_largest_size_in_bytes_and_mib() {
    let image = growable_image();

    let out = run(&["info"], image.path());

    assert_eq!(out.status.code(), Some(0), "stderr:\n{}", text(&out.stderr));
    let stdout = text(&out.stdout);
    assert!(
        stdout.contains(
            "\nSize:\n  Current size: 35840000 bytes (34.18 MiB)\n  Device sectors: 90000\n  \
             Can grow: Yes\n  Max new size: 46080000 bytes (43.95 MiB)\n"
        ),
        "got:\n{stdout}"
    );
}
