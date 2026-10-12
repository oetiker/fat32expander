//! [F-08]: the library reports what a resize does through events the caller
//! receives, and never writes to stderr itself. What reaches the terminal, and
//! on which stream, is the CLI's decision.
//!
//! The fixture is hand-built, so these run on every `cargo test`.

use fat32expander::{resize_fat32, resize_fat32_with_events, ResizeEvent, ResizeOptions};
use std::process::Command;
use tempfile::NamedTempFile;

const TOTAL_SECTORS: u32 = 70_000;
const DEVICE_SECTORS: usize = 90_000;
const RESERVED_SECTORS: usize = 32;
const FAT_SIZE: usize = 550;

/// A FAT32 volume of 70,000 sectors on a 90,000-sector file, with clusters 2
/// and 3 in use, so a resize grows the FAT and shifts two clusters.
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
        for (entry, value) in [0x0FFF_FFF8u32, 0x0FFF_FFFF, 0x0FFF_FFFF, 0x0FFF_FFFF]
            .iter()
            .enumerate()
        {
            data[at + entry * 4..at + entry * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    let file = NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), &data).expect("write image");
    file
}

fn collect_events(verbose: bool) -> Vec<ResizeEvent> {
    let image = growable_image();
    let mut events = Vec::new();
    resize_fat32_with_events(
        ResizeOptions::new(image.path()).verbose(verbose),
        &mut |e| events.push(e.clone()),
    )
    .expect("resize must succeed");
    events
}

#[test]
fn a_verbose_resize_reports_the_layout_the_plan_and_every_move() {
    let events = collect_events(true);

    assert!(
        events.iter().any(|e| matches!(
            e,
            ResizeEvent::Layout {
                old_total_sectors: TOTAL_SECTORS,
                fat_needs_growth: true,
                ..
            }
        )),
        "{events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ResizeEvent::ShiftPlanned { clusters: 2 })),
        "{events:#?}"
    );
    let moved: Vec<u32> = events
        .iter()
        .filter_map(|e| match e {
            ResizeEvent::ClusterMoved { cluster, .. } => Some(*cluster),
            _ => None,
        })
        .collect();
    assert_eq!(
        moved,
        vec![3, 2],
        "moves are reported highest cluster first"
    );
}

#[test]
fn a_quiet_resize_reports_its_steps_but_no_detail() {
    let events = collect_events(false);

    assert!(
        events.iter().all(|e| matches!(e, ResizeEvent::Step(_))),
        "without verbose only steps are reported: {events:#?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, ResizeEvent::Step(s) if s == "Verified device is not mounted")),
        "{events:#?}"
    );
}

#[test]
fn the_steps_reported_are_the_operations_returned() {
    let image = growable_image();
    let mut steps = Vec::new();
    let result = resize_fat32_with_events(ResizeOptions::new(image.path()), &mut |e| {
        if let ResizeEvent::Step(s) = e {
            steps.push(s.clone());
        }
    })
    .expect("resize must succeed");
    assert_eq!(steps, result.operations);
}

/// The child half of [`the_library_writes_nothing_to_stderr`]. It does nothing
/// unless that test started it.
#[test]
fn child_runs_a_verbose_resize() {
    if std::env::var_os("FAT32EXPANDER_STDERR_CHILD").is_none() {
        return;
    }
    let image = growable_image();
    resize_fat32(ResizeOptions::new(image.path()).verbose(true)).expect("resize must succeed");
}

/// The library used to `eprintln!` its progress, so a program embedding it
/// could neither capture nor silence it. The resize runs in a child process
/// so its stderr can be inspected; `--nocapture` hands it the real stream.
#[test]
fn the_library_writes_nothing_to_stderr() {
    let out = Command::new(std::env::current_exe().expect("test binary path"))
        .args(["--exact", "child_runs_a_verbose_resize", "--nocapture"])
        .env("FAT32EXPANDER_STDERR_CHILD", "1")
        .output()
        .expect("run child");
    assert!(
        out.status.success(),
        "child failed:\n{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.trim().is_empty(),
        "the library wrote to stderr:\n{stderr}"
    );
}

/// Sectors on the device of `image_beyond_the_fat32_limit`: more than the
/// `u32` sector count of a FAT32 boot sector can name.
const BEYOND_LIMIT_SECTORS: u64 = u32::MAX as u64 + 4096;

/// A FAT32 volume of 65,600 clusters of 64 sectors on a sparse file of
/// `BEYOND_LIMIT_SECTORS` sectors. Only the first sectors are written.
///
/// `None` when the filesystem under the temp directory refuses a file over
/// 2 TiB (ext4 without the `huge_file` feature does).
fn image_beyond_the_fat32_limit() -> Option<NamedTempFile> {
    const CLUSTERS: usize = 65_600;
    const SPC: usize = 64;
    let fat_size = ((CLUSTERS + 2) * 4).div_ceil(512);
    let total_sectors = (RESERVED_SECTORS + 2 * fat_size + CLUSTERS * SPC) as u32;

    let mut data = vec![0u8; (RESERVED_SECTORS + 2 * fat_size) * 512];
    let boot = &mut data[0..512];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MSDOS5.0");
    boot[11..13].copy_from_slice(&512u16.to_le_bytes());
    boot[13] = SPC as u8;
    boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
    boot[16] = 2; // number of FATs
    boot[21] = 0xF8;
    boot[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    boot[36..40].copy_from_slice(&(fat_size as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    boot[48] = 1; // FSInfo sector
    boot[50] = 6; // backup boot sector
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
        let at = (RESERVED_SECTORS + fat * fat_size) * 512;
        for (entry, value) in [0x0FFF_FFF8u32, 0x0FFF_FFFF, 0x0FFF_FFFF]
            .iter()
            .enumerate()
        {
            data[at + entry * 4..at + entry * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    let file = NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), &data).expect("write image");
    match file.as_file().set_len(BEYOND_LIMIT_SECTORS * 512) {
        Ok(()) => Some(file),
        Err(e) if e.kind() == std::io::ErrorKind::FileTooLarge => None,
        Err(e) => panic!("grow the sparse image: {e}"),
    }
}

/// A device with more sectors than FAT32 can address used to be refused as
/// an internal failure (exit 1). The volume now grows to the largest size
/// FAT32 can describe, and the resize says the rest of the device stays
/// unused.
#[test]
fn a_device_beyond_the_fat32_limit_grows_the_volume_to_that_limit() {
    // Skipping where the host cannot hold the image keeps local runs green,
    // but a skip in CI would hide a regression, so CI fails instead.
    let Some(image) = image_beyond_the_fat32_limit() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "the temp directory cannot hold a file over 2 TiB; set TMPDIR to one that can"
        );
        eprintln!("SKIPPED: the temp directory cannot hold a file over 2 TiB");
        return;
    };
    let mut events = Vec::new();
    let result =
        resize_fat32_with_events(ResizeOptions::new(image.path()).dry_run(true), &mut |e| {
            events.push(e.clone())
        })
        .expect("a device beyond the FAT32 limit must not be refused");

    assert_eq!(result.new_total_sectors, u32::MAX);
    assert!(
        events.contains(&ResizeEvent::DeviceBeyondFat32Limit {
            device_sectors: BEYOND_LIMIT_SECTORS,
            used_sectors: u32::MAX,
        }),
        "the resize must say the rest of the device stays unused: {events:?}"
    );
}
