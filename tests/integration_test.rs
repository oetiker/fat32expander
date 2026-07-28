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

    assert_eq!(info.bytes_per_sector, 512);
    assert!(info.data_clusters > 65525, "Should be FAT32");
    assert!(!info.can_grow, "Same size should not be growable");
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
    assert!(info_before.can_grow);

    // Resize
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    let result = resize_fat32(options).expect("Resize failed");

    assert!(result.new_size_bytes > result.old_size_bytes);

    // Get info after resize
    let info_after = get_fs_info(image.path()).expect("Failed to get fs info");
    assert!(!info_after.can_grow, "Should be at max size now");

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
    assert!(info_before.can_grow);

    // Resize
    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(true);
    let result = resize_fat32(options).expect("Resize failed");

    assert!(result.new_size_bytes > result.old_size_bytes);
    // FAT growth depends on the size increase

    // Get info after resize
    let info_after = get_fs_info(image.path()).expect("Failed to get fs info");
    assert!(!info_after.can_grow, "Should be at max size now");
    assert!(
        info_after.data_clusters > info_before.data_clusters,
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

    let added_clusters = info_after.data_clusters - info_before.data_clusters;
    assert!(added_clusters > 0, "resize should add clusters");
    assert_eq!(
        info_after.free_clusters,
        info_before.free_clusters + added_clusters,
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
    assert_eq!(info_before.total_sectors, info_after.total_sectors);
    assert_eq!(info_before.fat_size_sectors, info_after.fat_size_sectors);
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
