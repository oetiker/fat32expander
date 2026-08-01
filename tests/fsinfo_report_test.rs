//! Safety-net tests for `get_fs_info` and `FSInfoReport`.
//!
//! Written before the [F-R42] (split `executor.rs`), [F-R43] (stop mirroring
//! `BootSector` field-for-field) and [F-R5] (make the query recovery-aware)
//! fixes.
//!
//! Two deliberate choices:
//!
//! * These go through the crate root (`fat32expander::get_fs_info`) and never
//!   name a module, so [F-R42]'s module split cannot break them by construction
//!   -- if a re-export is dropped, that is a real API break and should fail.
//! * Every one of the report's 17 fields is asserted against a hand-built
//!   geometry with independently computed expectations. [F-R43] rewrites all 17
//!   as delegating accessors; before this file only 4 of them were covered
//!   anywhere, so an accessor delegating to the wrong `BootSector` method would
//!   have gone unnoticed.
//!
//! No `mkfs.fat` is needed, so unlike `integration_test.rs` these are not
//! `#[ignore]`d and run on every `cargo test`.

use fat32expander::get_fs_info;
use tempfile::NamedTempFile;

/// Render a filesystem report the way a user sees it.
///
/// [F-R42] moved rendering off `FSInfoReport` and onto the CLI, so these
/// assertions now run against the binary's actual output instead of a library
/// `Display` impl. The facts asserted are unchanged -- only where the string
/// comes from moved, and it moved closer to what a user actually reads.
fn rendered_report(path: &std::path::Path) -> String {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fat32expander"))
        .arg("info")
        .arg(path)
        .output()
        .expect("failed to run fat32expander");
    assert!(
        output.status.success(),
        "info command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// Geometry of the fixture. Chosen so the volume is unambiguously FAT32
// (data clusters well above the 65525 floor) while staying small enough for a
// temp file, and so that every field has a distinct value -- a report that
// mixed up two fields would still be caught.
const BYTES_PER_SECTOR: u16 = 512;
const SECTORS_PER_CLUSTER: u8 = 1;
const RESERVED_SECTORS: u16 = 32;
const NUM_FATS: u8 = 2;
const FAT_SIZE: u32 = 550;
const TOTAL_SECTORS: u32 = 70_000;
const ROOT_CLUSTER: u32 = 2;
const FSINFO_SECTOR: u16 = 1;
const BACKUP_BOOT_SECTOR: u16 = 6;
const FREE_CLUSTERS: u32 = 12_345;

/// Data clusters, derived here rather than read back from the code under test.
const EXPECTED_DATA_CLUSTERS: u32 =
    (TOTAL_SECTORS - RESERVED_SECTORS as u32 - (NUM_FATS as u32 * FAT_SIZE))
        / SECTORS_PER_CLUSTER as u32;

fn boot_sector_bytes() -> [u8; 512] {
    let mut data = [0u8; 512];
    data[0] = 0xEB;
    data[1] = 0x58;
    data[2] = 0x90;
    data[3..11].copy_from_slice(b"MSDOS5.0");
    data[11..13].copy_from_slice(&BYTES_PER_SECTOR.to_le_bytes());
    data[13] = SECTORS_PER_CLUSTER;
    data[14..16].copy_from_slice(&RESERVED_SECTORS.to_le_bytes());
    data[16] = NUM_FATS;
    data[21] = 0xF8;
    data[32..36].copy_from_slice(&TOTAL_SECTORS.to_le_bytes());
    data[36..40].copy_from_slice(&FAT_SIZE.to_le_bytes());
    data[44..48].copy_from_slice(&ROOT_CLUSTER.to_le_bytes());
    data[48..50].copy_from_slice(&FSINFO_SECTOR.to_le_bytes());
    data[50..52].copy_from_slice(&BACKUP_BOOT_SECTOR.to_le_bytes());
    data[82..90].copy_from_slice(b"FAT32   ");
    data[510] = 0x55;
    data[511] = 0xAA;
    data
}

fn fsinfo_sector_bytes(free_clusters: u32) -> [u8; 512] {
    let mut data = [0u8; 512];
    data[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes()); // lead signature
    data[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes()); // struct signature
    data[488..492].copy_from_slice(&free_clusters.to_le_bytes());
    data[492..496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // next free: unknown
    data[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes()); // trail signature
    data
}

/// Build an image of `device_sectors` sectors holding the fixture filesystem.
fn fixture_image(device_sectors: u64, free_clusters: u32, matching_backup: bool) -> NamedTempFile {
    let file = NamedTempFile::new().expect("temp file");
    let mut data = vec![0u8; device_sectors as usize * BYTES_PER_SECTOR as usize];

    let boot = boot_sector_bytes();
    data[0..512].copy_from_slice(&boot);

    let fsinfo_at = FSINFO_SECTOR as usize * 512;
    data[fsinfo_at..fsinfo_at + 512].copy_from_slice(&fsinfo_sector_bytes(free_clusters));

    let backup_at = BACKUP_BOOT_SECTOR as usize * 512;
    let mut backup = boot;
    if !matching_backup {
        // Diverge in a field `boot_sectors_match` compares, leaving the
        // signature valid so the volume still reads.
        backup[13] = SECTORS_PER_CLUSTER + 1;
    }
    data[backup_at..backup_at + 512].copy_from_slice(&backup);

    std::fs::write(file.path(), &data).expect("write image");
    file
}

/// A device with room to grow: the filesystem declares 70000 sectors, the
/// backing file has 90000.
fn growable_image() -> NamedTempFile {
    fixture_image(90_000, FREE_CLUSTERS, true)
}

/// Every field of the report, against independently computed expectations.
///
/// [F-R43] replaces the 14 mirrored fields with accessors delegating to
/// `BootSector`. This is what catches an accessor wired to the wrong method --
/// the values are spelled out here, not re-derived from the same code that
/// produces them.
#[test]
fn report_fields_match_a_known_geometry() {
    let image = growable_image();
    let info = get_fs_info(image.path()).expect("fixture must be a valid FAT32 filesystem");

    assert_eq!(info.device_path(), image.path(), "device_path");
    assert_eq!(
        info.boot().bytes_per_sector(),
        BYTES_PER_SECTOR,
        "bytes_per_sector"
    );
    assert_eq!(
        info.boot().sectors_per_cluster(),
        SECTORS_PER_CLUSTER,
        "sectors_per_cluster"
    );
    assert_eq!(
        info.boot().reserved_sectors(),
        RESERVED_SECTORS,
        "reserved_sectors"
    );
    assert_eq!(info.boot().num_fats(), NUM_FATS, "num_fats");
    assert_eq!(info.boot().fat_size(), FAT_SIZE, "fat_size_sectors");
    assert_eq!(info.boot().total_sectors(), TOTAL_SECTORS, "total_sectors");
    assert_eq!(
        info.data_clusters(),
        EXPECTED_DATA_CLUSTERS,
        "data_clusters"
    );
    assert_eq!(info.boot().root_cluster(), ROOT_CLUSTER, "root_cluster");
    assert_eq!(info.boot().fs_info_sector(), FSINFO_SECTOR, "fsinfo_sector");
    assert_eq!(
        info.boot().backup_boot_sector(),
        BACKUP_BOOT_SECTOR,
        "backup_boot_sector"
    );
    assert_eq!(info.free_clusters(), FREE_CLUSTERS, "free_clusters");
    assert!(info.backup_matches(), "backup_matches");
    assert_eq!(info.device_sectors(), 90_000, "device_sectors");
    assert!(info.can_grow(), "can_grow");
    assert_eq!(
        info.current_size_bytes(),
        TOTAL_SECTORS as u64 * BYTES_PER_SECTOR as u64,
        "current_size_bytes"
    );
    assert_eq!(
        info.max_new_size_bytes(),
        Some(90_000 * BYTES_PER_SECTOR as u64),
        "max_new_size_bytes"
    );

    // Guard the fixture itself: if this stops being FAT32, the test above is
    // asserting against a volume the tool should have rejected.
    assert!(
        info.data_clusters() >= 65525,
        "fixture must be FAT32, got {} clusters",
        info.data_clusters()
    );
}

/// The rendered report must carry the facts a user reads it for.
///
/// Asserted as *content*, not layout, so cosmetic formatting changes are free.
/// [F-R42] moved this rendering onto the CLI, so the string now comes from the
/// binary rather than from a library `Display` impl. The facts asserted below
/// are unchanged from when they were written against `FSInfoReport::to_string`.
#[test]
fn display_output_reports_every_key_fact() {
    let image = growable_image();
    let rendered = rendered_report(image.path());

    for (label, needle) in [
        ("device path", image.path().display().to_string()),
        ("bytes per sector", BYTES_PER_SECTOR.to_string()),
        ("reserved sectors", RESERVED_SECTORS.to_string()),
        ("fat size", FAT_SIZE.to_string()),
        ("total sectors", TOTAL_SECTORS.to_string()),
        ("data clusters", EXPECTED_DATA_CLUSTERS.to_string()),
        ("root cluster", ROOT_CLUSTER.to_string()),
        ("free clusters", FREE_CLUSTERS.to_string()),
        ("device sectors", 90_000.to_string()),
        ("current size", (TOTAL_SECTORS as u64 * 512).to_string()),
    ] {
        assert!(
            rendered.contains(&needle),
            "rendered report omits the {label} ({needle}).\nrendered:\n{rendered}"
        );
    }

    // A growable filesystem must say so, and offer the larger size.
    assert!(
        rendered.contains("Can grow: Yes"),
        "a growable filesystem must report that it can grow.\nrendered:\n{rendered}"
    );
    assert!(
        rendered.contains("Max new size"),
        "a growable filesystem must report its maximum size.\nrendered:\n{rendered}"
    );
}

/// A filesystem that already fills its device reports no headroom. `main.rs`
/// reads exactly these two values to decide whether to refuse the resize, so
/// they must not drift.
#[test]
fn a_filesystem_filling_its_device_reports_no_headroom() {
    // Device is exactly the size the filesystem declares.
    let image = fixture_image(TOTAL_SECTORS as u64, FREE_CLUSTERS, true);
    let info = get_fs_info(image.path()).expect("fixture must be valid");

    assert!(
        !info.can_grow(),
        "a filesystem filling its device cannot grow"
    );
    assert_eq!(
        info.max_new_size_bytes(),
        None,
        "there is no larger size to offer"
    );
    assert_eq!(
        info.current_size_bytes(),
        TOTAL_SECTORS as u64 * BYTES_PER_SECTOR as u64
    );
    assert!(rendered_report(image.path()).contains("Can grow: No"));
}

/// A mismatched backup boot sector is reported as a fact, not raised as an
/// error -- the CLI turns it into a warning overridable with
/// `--ignore-backup-mismatch`, so `get_fs_info` must keep succeeding.
#[test]
fn a_mismatched_backup_boot_sector_is_reported_not_raised() {
    let image = fixture_image(90_000, FREE_CLUSTERS, false);
    let info = get_fs_info(image.path()).expect("a backup mismatch must not fail the query");

    assert!(!info.backup_matches(), "the mismatch must be reported");
    assert!(
        rendered_report(image.path()).contains("Backup matches primary: NO"),
        "the rendered report must surface the mismatch"
    );
}

/// FAT32 encodes "free count unknown" as 0xFFFFFFFF. It must not be rendered
/// as four billion free clusters.
#[test]
fn an_unknown_free_count_is_reported_as_unknown() {
    let image = fixture_image(90_000, 0xFFFF_FFFF, true);
    let info = get_fs_info(image.path()).expect("fixture must be valid");

    assert_eq!(info.free_clusters(), 0xFFFF_FFFF);
    let rendered = rendered_report(image.path());
    assert!(
        rendered.contains("Free clusters: Unknown"),
        "an unknown free count must render as Unknown.\nrendered:\n{rendered}"
    );
    assert!(
        !rendered.contains("4294967295"),
        "the sentinel must not leak into the rendered report.\nrendered:\n{rendered}"
    );
}
