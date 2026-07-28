//! Safety-net tests pinning the crate's load-bearing public API surface.
//!
//! Written before the [F-R44] (collapse the three public paths per type and
//! stop exporting unusable items), [F-R34] (delete seven dead public FAT
//! functions) and [F-R35] (delete the unused `ClusterId`/`SectorNum` newtypes)
//! fixes. All three narrow what the library exports, and a visibility change is
//! the one kind of edit whose damage is invisible to the crate's own unit tests
//! -- `src/` can reach everything regardless of what `pub` says.
//!
//! What this file pins is deliberately *not* the whole current surface. Pinning
//! everything would forbid the fixes it exists to protect. It pins the paths
//! that are already load-bearing:
//!
//! * the crate root, which `tests/fsinfo_report_test.rs` uses exclusively, and
//! * `fat32expander::fat32::…`, which `tests/integration_test.rs` uses at three
//!   call sites.
//!
//! Everything not named here is fair game for [F-R44] to narrow. Everything
//! named here must still compile and behave identically afterwards. The items
//! [F-R34] and [F-R35] delete are deliberately absent, so those deletions
//! cannot break this file -- if a fix removes something listed below, that is a
//! real API break and should fail loudly.
//!
//! The fixture is hand-built rather than produced by `mkfs.fat`, so unlike
//! `integration_test.rs` these are not `#[ignore]`d and run on every
//! `cargo test`.

use fat32expander::{
    check_not_mounted, check_root, get_block_device_size, get_fs_info, resize_fat32, BootSector,
    Device, Error, FSInfo, FSInfoReport, ResizeOptions, ResizeResult,
};
use tempfile::NamedTempFile;

// Geometry of the fixture. Distinct values per field, so a re-export wired to
// the wrong item would still be caught rather than coincidentally matching.
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

/// The device is larger than the filesystem, so the volume can grow.
const DEVICE_SECTORS: u64 = 90_000;

/// Derived here rather than read back from the code under test.
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

fn fsinfo_sector_bytes() -> [u8; 512] {
    let mut data = [0u8; 512];
    data[0..4].copy_from_slice(&FSInfo::LEAD_SIG.to_le_bytes());
    data[484..488].copy_from_slice(&FSInfo::STRUC_SIG.to_le_bytes());
    data[488..492].copy_from_slice(&FREE_CLUSTERS.to_le_bytes());
    data[492..496].copy_from_slice(&FSInfo::UNKNOWN_FREE.to_le_bytes());
    data[508..512].copy_from_slice(&FSInfo::TRAIL_SIG.to_le_bytes());
    data
}

/// An image holding the fixture filesystem, with both FATs marking clusters
/// 2 and 3 as an end-of-chain so the FAT is not uniformly zero.
fn fixture_image() -> NamedTempFile {
    let file = NamedTempFile::new().expect("temp file");
    let mut data = vec![0u8; DEVICE_SECTORS as usize * BYTES_PER_SECTOR as usize];

    let boot = boot_sector_bytes();
    data[0..512].copy_from_slice(&boot);

    let fsinfo_at = FSINFO_SECTOR as usize * 512;
    data[fsinfo_at..fsinfo_at + 512].copy_from_slice(&fsinfo_sector_bytes());

    let backup_at = BACKUP_BOOT_SECTOR as usize * 512;
    data[backup_at..backup_at + 512].copy_from_slice(&boot);

    // Populate both FAT copies: entries 0 and 1 are the reserved media/EOC
    // markers, 2 and 3 are in use.
    for fat in 0..NUM_FATS as u32 {
        let fat_at = (RESERVED_SECTORS as usize + (fat as usize * FAT_SIZE as usize)) * 512;
        for (entry, value) in [
            (0usize, 0x0FFF_FFF8u32),
            (1, 0x0FFF_FFFF),
            (2, 0x0FFF_FFFF),
            (3, 0x0FFF_FFFF),
        ] {
            let at = fat_at + entry * 4;
            data[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    std::fs::write(file.path(), &data).expect("write image");
    file
}

/// `get_fs_info` and `FSInfoReport` must stay reachable from the crate root and
/// keep answering the same facts. This is the read-only half of the library's
/// contract, and the only path `tests/fsinfo_report_test.rs` uses.
#[test]
fn the_crate_root_query_api_reports_the_fixture_geometry() {
    let image = fixture_image();

    let info: FSInfoReport = get_fs_info(image.path()).expect("fixture must be valid FAT32");

    assert_eq!(info.device_path(), image.path(), "device_path");
    assert_eq!(
        info.data_clusters(),
        EXPECTED_DATA_CLUSTERS,
        "data_clusters"
    );
    assert_eq!(info.free_clusters(), FREE_CLUSTERS, "free_clusters");
    assert!(info.backup_matches(), "backup_matches");
    assert!(info.can_grow(), "device is larger than the filesystem");
    assert_eq!(
        info.current_size_bytes(),
        TOTAL_SECTORS as u64 * BYTES_PER_SECTOR as u64,
        "current_size_bytes"
    );
}

/// `BootSector` must stay reachable from the crate root, and the report must
/// keep handing it out rather than mirroring its fields.
#[test]
fn the_crate_root_exposes_the_boot_sector_behind_the_report() {
    let image = fixture_image();

    let info = get_fs_info(image.path()).expect("fixture must be valid FAT32");
    let boot: &BootSector = info.boot();

    assert_eq!(boot.bytes_per_sector(), BYTES_PER_SECTOR);
    assert_eq!(boot.sectors_per_cluster(), SECTORS_PER_CLUSTER);
    assert_eq!(boot.reserved_sectors(), RESERVED_SECTORS);
    assert_eq!(boot.num_fats(), NUM_FATS);
    assert_eq!(boot.fat_size(), FAT_SIZE);
    assert_eq!(boot.total_sectors(), TOTAL_SECTORS);
    assert_eq!(boot.root_cluster(), ROOT_CLUSTER);
    assert!(boot.is_signature_valid());
}

/// `ResizeOptions`, `resize_fat32` and `ResizeResult` must stay reachable from
/// the crate root. A dry run keeps this test from mutating the fixture while
/// still driving the whole entry point.
#[test]
fn the_crate_root_resize_api_plans_a_growth_without_writing() {
    let image = fixture_image();
    let before = std::fs::read(image.path()).expect("read image");

    let options: ResizeOptions = ResizeOptions::new(image.path())
        .dry_run(true)
        .verbose(false);
    assert_eq!(options.device_path(), image.path());
    assert!(options.is_dry_run());
    assert!(!options.is_verbose());

    let result: ResizeResult = resize_fat32(options).expect("dry run must succeed");

    assert_eq!(
        result.calculation.old_total_sectors, TOTAL_SECTORS,
        "result must report the filesystem it started from"
    );
    assert_eq!(
        result.old_size_bytes,
        TOTAL_SECTORS as u64 * BYTES_PER_SECTOR as u64,
        "old_size_bytes"
    );
    assert!(
        result.new_size_bytes > result.old_size_bytes,
        "a growable filesystem must plan a larger volume, got {} -> {}",
        result.old_size_bytes,
        result.new_size_bytes
    );

    let after = std::fs::read(image.path()).expect("read image");
    assert!(before == after, "a dry run must not modify the image");
}

/// The `fat32` module path must keep exporting the device opener, the boot
/// sector reader, the FAT reader and the `fat_entry` helpers.
/// `tests/integration_test.rs` imports exactly these four at three call sites.
#[test]
fn the_fat32_module_path_opens_a_device_and_reads_its_metadata() {
    use fat32expander::fat32::{fat_entry, open_fat32_device, read_boot_sector, read_fat_table};

    let image = fixture_image();

    let device = open_fat32_device(image.path(), false).expect("open fixture");
    assert_eq!(device.sector_size(), BYTES_PER_SECTOR as u32);
    assert_eq!(device.total_sectors(), DEVICE_SECTORS);

    let boot = read_boot_sector(&device).expect("read boot sector");
    assert_eq!(boot.fat_size(), FAT_SIZE);

    let fat = read_fat_table(&device, &boot, 0).expect("read FAT");
    assert_eq!(
        fat.len(),
        FAT_SIZE as usize * BYTES_PER_SECTOR as usize / 4,
        "FAT must read back in full"
    );

    assert!(fat_entry::is_end_of_chain(fat[2]), "cluster 2 is in use");
    assert!(fat_entry::is_end_of_chain(fat[3]), "cluster 3 is in use");
    assert!(fat_entry::is_free(fat[4]), "cluster 4 is free");
}

/// `Device` must stay reachable from the crate root and keep its sector-
/// addressed reads. The library is usable as a library only if a caller can
/// open a volume itself.
#[test]
fn the_crate_root_exposes_the_device_type() {
    let image = fixture_image();

    let device: Device =
        Device::open_readonly_with_sector_size(image.path(), 512).expect("open fixture read-only");

    assert_eq!(device.path(), image.path());
    assert_eq!(device.sector_size(), 512);
    assert_eq!(device.total_sectors(), DEVICE_SECTORS);

    let sector = device.read_sector(0).expect("read boot sector");
    assert_eq!(sector.len(), 512);
    assert_eq!(&sector[510..512], &[0x55, 0xAA], "boot signature");
}

/// `FSInfo` must stay reachable from the crate root, along with the signature
/// constants a caller needs to recognise one.
#[test]
fn the_crate_root_exposes_the_fsinfo_type_and_its_signatures() {
    let fsinfo = FSInfo::from_bytes(&fsinfo_sector_bytes()).expect("parse fixture FSInfo");

    assert_eq!(fsinfo.lead_sig(), FSInfo::LEAD_SIG);
    assert_eq!(fsinfo.struc_sig(), FSInfo::STRUC_SIG);
    assert_eq!(fsinfo.trail_sig(), FSInfo::TRAIL_SIG);
    assert_eq!(fsinfo.free_count(), FREE_CLUSTERS);
    assert_eq!(fsinfo.next_free(), FSInfo::UNKNOWN_FREE);
}

/// `Error` must stay reachable from the crate root and stay matchable by
/// variant. A caller that can only read the message string has no contract.
#[test]
fn the_crate_root_exposes_the_error_type_as_a_matchable_enum() {
    let missing = std::path::Path::new("/nonexistent/fat32expander/public-api-test");

    let err: Error = get_fs_info(missing).expect_err("a missing device must fail");

    assert!(
        matches!(err, Error::DeviceOpen { .. }),
        "expected DeviceOpen, got: {err:?}"
    );
    assert!(
        err.to_string().contains("nonexistent"),
        "the error must name the device: {err}"
    );
}

/// The `system` helpers must stay reachable from the crate root. A regular file
/// is never mounted and its size is answered by the seek fallback, so both can
/// be driven without a real block device.
#[test]
fn the_crate_root_exposes_the_system_helpers() {
    let image = fixture_image();

    check_not_mounted(image.path()).expect("a temp file is never mounted");

    let size = get_block_device_size(image.path()).expect("size a regular file");
    assert_eq!(size, DEVICE_SECTORS * BYTES_PER_SECTOR as u64);

    // Only that it is callable and total; which answer it gives depends on who
    // runs the suite.
    let _: bool = check_root();
}
