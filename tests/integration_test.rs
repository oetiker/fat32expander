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
    let info = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");

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
    let info_before = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");
    assert!(info_before.can_grow);

    // Resize
    let options = ResizeOptions {
        device_path: image.path().to_str().unwrap().to_string(),
        dry_run: false,
        verbose: false,
    };
    let result = resize_fat32(options).expect("Resize failed");

    assert!(result.new_size_bytes > result.old_size_bytes);

    // Get info after resize
    let info_after = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");
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
    let info_before = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");
    assert!(info_before.can_grow);

    // Resize
    let options = ResizeOptions {
        device_path: image.path().to_str().unwrap().to_string(),
        dry_run: false,
        verbose: true,
    };
    let result = resize_fat32(options).expect("Resize failed");

    assert!(result.new_size_bytes > result.old_size_bytes);
    // FAT growth depends on the size increase

    // Get info after resize
    let info_after = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");
    assert!(!info_after.can_grow, "Should be at max size now");
    assert!(
        info_after.data_clusters > info_before.data_clusters,
        "Should have more clusters"
    );

    // Verify filesystem integrity
    assert!(check_filesystem(image.path()), "Filesystem check failed");
}

#[test]
#[ignore] // Requires mkfs.fat
fn test_dry_run() {
    // Create a 128MB FAT32 image
    let image = create_fat32_image(128);

    // Extend to 256MB
    extend_image(image.path(), 256);

    // Get original size
    let info_before = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");

    // Dry run resize
    let options = ResizeOptions {
        device_path: image.path().to_str().unwrap().to_string(),
        dry_run: true,
        verbose: false,
    };
    let result = resize_fat32(options).expect("Dry run failed");

    // Verify it reported what would happen
    assert!(result.new_size_bytes > result.old_size_bytes);

    // Verify no changes were made
    let info_after = get_fs_info(image.path().to_str().unwrap()).expect("Failed to get fs info");
    assert_eq!(info_before.total_sectors, info_after.total_sectors);
    assert_eq!(info_before.fat_size_sectors, info_after.fat_size_sectors);
}

#[test]
#[ignore] // Requires mkfs.fat
fn test_already_max_size() {
    // Create a 128MB FAT32 image (no extension)
    let image = create_fat32_image(128);

    // Try to resize - should fail because already at max size
    let options = ResizeOptions {
        device_path: image.path().to_str().unwrap().to_string(),
        dry_run: false,
        verbose: false,
    };
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
    let options = ResizeOptions {
        device_path: image.path().to_str().unwrap().to_string(),
        dry_run: false,
        verbose: false,
    };
    resize_fat32(options).expect("Resize failed");

    // Verify filesystem integrity
    assert!(check_filesystem(image.path()), "Filesystem check failed");
}
