use fat32expander::{get_fs_info, resize_fat32, ResizeOptions};
use std::process::Command;
use tempfile::NamedTempFile;

/// Create a FAT32 test image of the specified size in MB
fn create_fat32_image(size_mb: u32) -> NamedTempFile {
    let file = NamedTempFile::new().expect("Failed to create temp file");
    let path = file.path();

    // Create file of specified size
    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", size_mb))
        .arg(path)
        .status()
        .expect("Failed to truncate file");

    // Format as FAT32
    let status = Command::new("mkfs.fat")
        .arg("-F")
        .arg("32")
        .arg(path)
        .status()
        .expect("Failed to run mkfs.fat");

    assert!(status.success(), "mkfs.fat failed");

    file
}

/// Extend an image file to a larger size
fn extend_image(path: &std::path::Path, new_size_mb: u32) {
    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", new_size_mb))
        .arg(path)
        .status()
        .expect("Failed to extend file");
}

/// Run dosfsck on an image and check for errors
fn check_filesystem(path: &std::path::Path) -> bool {
    let output = Command::new("dosfsck")
        .arg("-n") // don't repair, just check
        .arg(path)
        .output()
        .expect("Failed to run dosfsck");

    // dosfsck returns 0 if no errors, non-zero otherwise
    output.status.success()
}

#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn test_info_command() {
    let image = create_fat32_image(128);
    let info = get_fs_info(image.path()).expect("Failed to get fs info");

    assert_eq!(info.boot().bytes_per_sector(), 512);
    assert!(info.data_clusters() > 65525, "Should be FAT32");
    assert!(!info.can_grow(), "Same size should not be growable");
}

#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn test_resize_without_fat_growth() {
    // Create a 128MB FAT32 image
    let image = create_fat32_image(128);

    // Extend to 150MB (small increase, shouldn't require FAT growth)
    extend_image(image.path(), 150);

    // Get info before resize
    let info_before = get_fs_info(image.path()).expect("Failed to get fs info");
    assert!(info_before.can_grow());

    // Resize
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    let result = resize_fat32(options).expect("Resize failed");

    assert!(result.new_size_bytes > result.old_size_bytes);

    // Get info after resize
    let info_after = get_fs_info(image.path()).expect("Failed to get fs info");
    assert!(!info_after.can_grow(), "Should be at max size now");

    // Verify filesystem integrity
    assert!(check_filesystem(image.path()), "Filesystem check failed");
}

#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn test_resize_with_fat_growth() {
    // Create a 128MB FAT32 image
    let image = create_fat32_image(128);

    // Extend to 256MB (larger increase, should require FAT growth)
    extend_image(image.path(), 256);

    // Get info before resize
    let info_before = get_fs_info(image.path()).expect("Failed to get fs info");
    assert!(info_before.can_grow());

    // Resize
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(true);
    let result = resize_fat32(options).expect("Resize failed");

    assert!(result.new_size_bytes > result.old_size_bytes);
    // FAT growth depends on the size increase

    // Get info after resize
    let info_after = get_fs_info(image.path()).expect("Failed to get fs info");
    assert!(!info_after.can_grow(), "Should be at max size now");
    assert!(
        info_after.data_clusters() > info_before.data_clusters(),
        "Should have more clusters"
    );

    // Verify filesystem integrity
    assert!(check_filesystem(image.path()), "Filesystem check failed");
}

/// Create a FAT32 test image with an explicit cluster size.
fn create_fat32_image_with_cluster_size(size_mb: u32, sectors_per_cluster: u32) -> NamedTempFile {
    let file = NamedTempFile::new().expect("Failed to create temp file");
    let path = file.path();

    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", size_mb))
        .arg(path)
        .status()
        .expect("Failed to truncate file");

    let status = Command::new("mkfs.fat")
        .arg("-F")
        .arg("32")
        .arg("-s")
        .arg(sectors_per_cluster.to_string())
        .arg(path)
        .status()
        .expect("Failed to run mkfs.fat");

    assert!(status.success(), "mkfs.fat failed");

    file
}

/// Copy a file into the image with mtools.
fn mcopy_into(image: &std::path::Path, source: &std::path::Path, dest_name: &str) {
    let status = Command::new("mcopy")
        .arg("-i")
        .arg(image)
        .arg(source)
        .arg(format!("::{}", dest_name))
        .status()
        .expect("Failed to run mcopy");
    assert!(status.success(), "mcopy failed for {}", dest_name);
}

/// Read a file back out of the image with mtools.
fn mcopy_out(image: &std::path::Path, name: &str) -> Vec<u8> {
    let out = tempfile::NamedTempFile::new().unwrap();
    let status = Command::new("mcopy")
        .arg("-i")
        .arg(image)
        .arg("-n")
        .arg(format!("::{}", name))
        .arg(out.path())
        .status()
        .expect("Failed to run mcopy");
    assert!(status.success(), "mcopy failed reading {}", name);
    std::fs::read(out.path()).unwrap()
}

/// Extend an image file to an exact size in KiB.
fn extend_image_kib(path: &std::path::Path, new_size_kib: u64) {
    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}K", new_size_kib))
        .arg(path)
        .status()
        .expect("Failed to extend file");
}

/// A geometry where the total FAT growth is not a whole number of clusters.
///
/// 256 MiB at 4 sectors per cluster gives a FAT of 1020 sectors; growing the
/// volume to 262400 KiB takes it to 1023, so the two FAT copies grow by 6
/// sectors in total - not a multiple of the 4-sector cluster. Rounding that up
/// to 2 whole clusters used to shift the data area 8 sectors instead of 6,
/// leaving every file 2 sectors past where the committed boot sector points.
///
/// Most geometries grow the FAT by a multiple of the cluster size, which is why
/// this went unnoticed; the increase here has to be small enough that the FAT
/// gains only a few sectors.
#[test]
#[ignore] // Requires mkfs.fat, dosfsck and mtools
fn test_resize_with_unaligned_fat_growth_preserves_files() {
    let image = create_fat32_image_with_cluster_size(256, 4);

    let payloads: Vec<(String, Vec<u8>)> = (0..20)
        .map(|i| {
            let name = format!("FILE{:03}.BIN", i);
            let content: Vec<u8> = (0..4096u32).map(|b| (b as u8) ^ (i as u8)).collect();
            (name, content)
        })
        .collect();

    for (name, content) in &payloads {
        let src = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(src.path(), content).unwrap();
        mcopy_into(image.path(), src.path(), name);
    }

    // 262400 KiB = 524800 sectors: FAT 1020 -> 1023, a 6-sector growth.
    extend_image_kib(image.path(), 262_400);

    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    let result = resize_fat32(options).expect("Resize failed");
    // Guard the geometry: 2 FATs x 3 sectors = 6 sectors of growth against a
    // 4-sector cluster. If mkfs.fat ever picks different numbers here, this test
    // stops exercising the unaligned path and needs revisiting.
    assert!(result.fat_grew, "this geometry must exercise FAT growth");
    assert_eq!(result.calculation.old_fat_size, 1020);
    assert_eq!(result.calculation.new_fat_size, 1023);

    assert!(
        check_filesystem(image.path()),
        "dosfsck reported errors after an unaligned FAT growth"
    );

    for (name, content) in &payloads {
        assert_eq!(
            &mcopy_out(image.path(), name),
            content,
            "{} did not survive the resize",
            name
        );
    }
}

/// The FSInfo free count is extrapolated from the executor's own copy of the
/// data-cluster formula. Pin the relationship it must hold to, since that copy
/// has no unit-level seam.
#[test]
#[ignore] // Requires mkfs.fat
fn test_free_cluster_count_tracks_added_clusters() {
    let image = create_fat32_image(128);
    extend_image(image.path(), 256);

    let info_before = get_fs_info(image.path()).expect("Failed to get fs info");

    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    resize_fat32(options).expect("Resize failed");

    let info_after = get_fs_info(image.path()).expect("Failed to get fs info");

    let added_clusters = info_after.data_clusters() - info_before.data_clusters();
    assert!(added_clusters > 0, "resize should add clusters");
    assert_eq!(
        info_after.free_clusters(),
        info_before.free_clusters() + added_clusters,
        "free count must grow by exactly the number of clusters added"
    );
}

#[test]
#[ignore] // Requires mkfs.fat
fn test_dry_run() {
    // Create a 128MB FAT32 image
    let image = create_fat32_image(128);

    // Extend to 256MB
    extend_image(image.path(), 256);

    // Get original size
    let info_before = get_fs_info(image.path()).expect("Failed to get fs info");

    // Dry run resize
    let options = ResizeOptions::new(image.path())
        .dry_run(true)
        .verbose(false);
    let result = resize_fat32(options).expect("Dry run failed");

    // Verify it reported what would happen
    assert!(result.new_size_bytes > result.old_size_bytes);

    // Verify no changes were made
    let info_after = get_fs_info(image.path()).expect("Failed to get fs info");
    assert_eq!(
        info_before.boot().total_sectors(),
        info_after.boot().total_sectors()
    );
    assert_eq!(info_before.boot().fat_size(), info_after.boot().fat_size());
}

#[test]
#[ignore] // Requires mkfs.fat
fn test_already_max_size() {
    // Create a 128MB FAT32 image (no extension)
    let image = create_fat32_image(128);

    // Try to resize - should fail because already at max size
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    let result = resize_fat32(options);

    assert!(result.is_err());
}

#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn test_resize_with_data() {
    // Create a 128MB FAT32 image
    let image = create_fat32_image(128);

    // Mount and add some files (if possible)
    // For now, we'll skip this as it requires sudo

    // Extend to 256MB
    extend_image(image.path(), 256);

    // Resize
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    resize_fat32(options).expect("Resize failed");

    // Verify filesystem integrity
    assert!(check_filesystem(image.path()), "Filesystem check failed");
}

// ---------------------------------------------------------------------------
// Safety-net tests written before the [F-R5]/[F-R6]/[F-R7]/[F-R8] fixes.
//
// These pin behaviour that must survive tightened validation and the removal of
// the CLI's substring-matched recovery entry. They assert what the tool does
// today on legitimate and on plainly-broken input; they are not tests of the
// fixes themselves.
// ---------------------------------------------------------------------------

/// Create a FAT32 image with explicit mkfs geometry.
fn create_fat32_image_with_sector_size(size_mb: u32, sector_size: u32, spc: u32) -> NamedTempFile {
    let file = NamedTempFile::new().expect("Failed to create temp file");
    let path = file.path();

    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", size_mb))
        .arg(path)
        .status()
        .expect("Failed to truncate file");

    let status = Command::new("mkfs.fat")
        .arg("-F")
        .arg("32")
        .arg("-S")
        .arg(sector_size.to_string())
        .arg("-s")
        .arg(spc.to_string())
        .arg(path)
        .status()
        .expect("Failed to run mkfs.fat");

    assert!(
        status.success(),
        "mkfs.fat failed for sector_size={sector_size} spc={spc}"
    );

    file
}

/// Real filesystems produced by `mkfs.fat` must keep validating and reporting
/// sane geometry across sector sizes. Guards the [F-R7]/[F-R8] fixes against
/// rejecting or mis-measuring legitimate volumes — the unit-level equivalent
/// uses synthetic boot sectors, this one uses what mkfs actually writes.
#[test]
#[ignore] // Requires mkfs.fat
fn mkfs_produced_filesystems_validate_across_sector_sizes() {
    // (size_mb, sector_size, sectors_per_cluster)
    //
    // Each case must clear FAT32's 65525-cluster floor. `mkfs.fat -F 32` will
    // happily format below it (128MB at spc=8 gives 32700 clusters), producing
    // a volume that is FAT16 by the spec's own definition and that this tool
    // rightly refuses — so the image has to be sized against the cluster size.
    let cases = [(128u32, 512u32, 1u32), (512, 512, 8), (512, 4096, 1)];

    for (size_mb, sector_size, spc) in cases {
        let image = create_fat32_image_with_sector_size(size_mb, sector_size, spc);
        let label = format!("size={size_mb}MB sector_size={sector_size} spc={spc}");

        let info = get_fs_info(image.path())
            .unwrap_or_else(|e| panic!("get_fs_info rejected a real filesystem ({label}): {e:?}"));

        assert_eq!(
            info.boot().bytes_per_sector() as u32,
            sector_size,
            "bps {label}"
        );
        assert_eq!(info.boot().sectors_per_cluster() as u32, spc, "spc {label}");
        assert!(
            info.data_clusters() >= 65525,
            "mkfs volume reported as non-FAT32 ({label}): {} clusters",
            info.data_clusters()
        );
        assert!(
            check_filesystem(image.path()),
            "dosfsck rejected a freshly made image ({label})"
        );
    }
}

/// The FAT of a valid image must read back in full: one entry per 4 bytes of
/// FAT, with cluster 2 (the root directory) allocated. Guards [F-R6]'s
/// allocation bound against being set tight enough to truncate a real FAT.
#[test]
#[ignore] // Requires mkfs.fat
fn fat_table_reads_back_in_full_for_a_valid_image() {
    use fat32expander::fat32::{fat_entry, read_boot_sector, read_fat_table};
    use fat32expander::Device;

    let image = create_fat32_image(128);
    let mut device = Device::open_readonly(image.path()).expect("open failed");
    let boot = read_boot_sector(&mut device).expect("read_boot_sector failed");

    let expected_entries = (boot.fat_size() as usize * boot.bytes_per_sector() as usize) / 4;
    let fat = read_fat_table(&device, &boot, 0).expect("read_fat_table failed");

    assert_eq!(
        fat.len(),
        expected_entries,
        "FAT entry count must cover the whole declared FAT"
    );
    assert!(
        !fat_entry::is_free(fat[boot.root_cluster() as usize]),
        "root cluster must be marked in use"
    );

    // The two FAT copies must agree on a freshly made filesystem.
    let fat2 = read_fat_table(&device, &boot, 1).expect("read_fat_table(1) failed");
    assert_eq!(fat, fat2, "FAT1 and FAT2 must match on a fresh image");
}

/// A device that is not FAT32 at all must be reported as an error. After
/// [F-R5] replaces the CLI's substring match with a typed error, this is what
/// stops an unrelated failure from being routed into crash recovery.
#[test]
#[ignore] // Requires mkfs.fat
fn non_fat32_device_is_rejected() {
    let file = NamedTempFile::new().expect("Failed to create temp file");
    Command::new("truncate")
        .arg("-s")
        .arg("64M")
        .arg(file.path())
        .status()
        .expect("truncate failed");

    // Zeroed image: no boot signature, no FAT32 anything.
    let result = get_fs_info(file.path());
    assert!(
        result.is_err(),
        "get_fs_info must reject a device that is not FAT32"
    );

    let options = ResizeOptions::new(file.path())
        .dry_run(false)
        .verbose(false);
    assert!(
        resize_fat32(options).is_err(),
        "resize_fat32 must reject a device that is not FAT32"
    );
}

/// An invalidated boot signature with no checkpoint on the device is not
/// recoverable, and must fail rather than be treated as a resumable resize.
/// [F-R5]'s fix changes how this reaches the CLI; the outcome must not change.
#[test]
#[ignore] // Requires mkfs.fat
fn invalidated_boot_sector_without_a_checkpoint_is_not_recoverable() {
    use std::io::{Read, Seek, SeekFrom, Write};

    let image = create_fat32_image(128);
    extend_image(image.path(), 256);

    // Zero the boot signature, exactly as the executor's danger zone does.
    {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(image.path())
            .expect("open for invalidation failed");
        f.seek(SeekFrom::Start(510)).expect("seek failed");
        f.write_all(&[0x00, 0x00]).expect("write failed");
        f.sync_all().expect("sync failed");

        let mut sig = [0u8; 2];
        f.seek(SeekFrom::Start(510)).expect("seek failed");
        f.read_exact(&mut sig).expect("read failed");
        assert_eq!(sig, [0x00, 0x00], "boot signature was not invalidated");
    }

    // get_fs_info uses the strict reader, so it must fail.
    assert!(
        get_fs_info(image.path()).is_err(),
        "get_fs_info must fail on an invalidated boot sector"
    );

    // resize_fat32 uses the recovery reader, finds no checkpoint, and must
    // refuse rather than proceed on a filesystem it cannot reconstruct.
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    assert!(
        resize_fat32(options).is_err(),
        "resize must refuse an invalidated filesystem with no checkpoint"
    );
}

// ===== Group A safety net: device geometry on real filesystems =====
//
// The unit tests in `device.rs` and `checkpoint.rs` pin the arithmetic. These
// pin it against filesystems `mkfs.fat` actually produced, at a sector size
// that is not the 512 `Device::open` hardcodes ([F-R23]) and that only the
// boot-sector reader's side effect ([F-R40]) currently supplies.

/// [F-R23]/[F-R40]: after the boot sector has been read, the device must
/// address sectors the way the filesystem does. The last sector -- where the
/// checkpoint goes -- must be readable at that geometry, and the sector count
/// must match the file's real size.
#[test]
#[ignore] // Requires mkfs.fat
fn device_geometry_matches_a_4096_byte_sector_filesystem() {
    use fat32expander::device::Device;
    use fat32expander::fat32::read_boot_sector;

    // Sized so each case clears FAT32's 65525-cluster floor and pushes the
    // sector count past 65535, or mkfs writes a FAT16-shaped boot sector.
    for (size_mb, sector_size) in [(128u32, 512u32), (512, 1024), (512, 2048), (512, 4096)] {
        let image = create_fat32_image_with_sector_size(size_mb, sector_size, 1);
        let len = std::fs::metadata(image.path()).unwrap().len();
        let label = format!("size={size_mb}MB sector_size={sector_size}");

        let mut device = Device::open(image.path()).expect("open failed");
        let boot = read_boot_sector(&mut device).expect("read_boot_sector failed");

        assert_eq!(
            boot.bytes_per_sector() as u32,
            sector_size,
            "mkfs geometry {label}"
        );
        assert_eq!(
            device.sector_size(),
            sector_size,
            "device must address sectors as the filesystem does, {label}"
        );
        assert_eq!(
            device.total_sectors(),
            len / sector_size as u64,
            "total_sectors must match the real device size, {label}"
        );

        // The checkpoint sector must be reachable at this geometry.
        let last = device.total_sectors() - 1;
        let data = device
            .read_sector(last)
            .unwrap_or_else(|e| panic!("last sector must be readable, {label}: {e}"));
        assert_eq!(
            data.len(),
            sector_size as usize,
            "short sector read {label}"
        );

        // And reading one past it must not silently succeed.
        assert!(
            device.read_sector(last + 1).is_err(),
            "reading past the last sector must fail, {label}"
        );
    }
}

/// [F-R40]: `read_boot_sector` is named as a reader but reconfigures the
/// device. Whether or not it keeps doing so, calling it twice must yield the
/// same boot sector -- a reader whose answer depends on how often it has been
/// called is not one.
#[test]
#[ignore] // Requires mkfs.fat
fn read_boot_sector_returns_the_same_answer_on_repeated_calls() {
    use fat32expander::device::Device;
    use fat32expander::fat32::read_boot_sector;

    for (size_mb, sector_size) in [(128u32, 512u32), (512, 4096)] {
        let image = create_fat32_image_with_sector_size(size_mb, sector_size, 1);
        let label = format!("size={size_mb}MB sector_size={sector_size}");

        let mut device = Device::open(image.path()).expect("open failed");
        let first = read_boot_sector(&mut device).expect("first read failed");
        let second = read_boot_sector(&mut device).expect("second read failed");

        assert_eq!(
            first.bytes_per_sector(),
            second.bytes_per_sector(),
            "bytes_per_sector drifted between calls, {label}"
        );
        assert_eq!(
            first.total_sectors(),
            second.total_sectors(),
            "total_sectors drifted between calls, {label}"
        );
        assert_eq!(
            first.fat_size(),
            second.fat_size(),
            "fat_size drifted between calls, {label}"
        );
        assert_eq!(
            first.reserved_sectors(),
            second.reserved_sectors(),
            "reserved_sectors drifted between calls, {label}"
        );
        assert_eq!(
            first.root_cluster(),
            second.root_cluster(),
            "root_cluster drifted between calls, {label}"
        );
        assert_eq!(
            device.sector_size(),
            sector_size,
            "the device must still address sectors correctly, {label}"
        );
    }
}

/// The end-to-end net for the whole group: a real resize, with FAT growth and
/// a file to preserve, on a filesystem whose sector size is not 512. Every
/// Group A fix changes how the device establishes or bounds its geometry, and
/// this is the case where a mistake in any of them shows up as corruption
/// rather than as a failing assertion.
#[test]
#[ignore] // Requires mkfs.fat, dosfsck and mtools
fn resize_preserves_files_on_a_4096_byte_sector_filesystem() {
    let image = create_fat32_image_with_sector_size(512, 4096, 1);

    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let source = NamedTempFile::new().unwrap();
    std::fs::write(source.path(), &payload).unwrap();
    mcopy_into(image.path(), source.path(), "PAYLOAD.BIN");

    // 512 MB -> 1024 MB doubles the cluster count, forcing the FAT to grow
    // (128 -> 256 sectors at this geometry) and so a data shift.
    extend_image(image.path(), 1024);

    let info_before = get_fs_info(image.path()).expect("info before failed");
    assert_eq!(info_before.boot().bytes_per_sector(), 4096);
    assert!(info_before.can_grow());

    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    let result = resize_fat32(options).expect("resize failed");
    assert!(result.new_size_bytes > result.old_size_bytes);

    assert!(
        check_filesystem(image.path()),
        "dosfsck failed after resizing a 4096-byte-sector filesystem"
    );

    let info_after = get_fs_info(image.path()).expect("info after failed");
    assert_eq!(
        info_after.boot().bytes_per_sector(),
        4096,
        "the sector size must not have been rewritten"
    );

    assert_eq!(
        mcopy_out(image.path(), "PAYLOAD.BIN"),
        payload,
        "the file must survive the resize byte for byte"
    );
}
