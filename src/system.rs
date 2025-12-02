use crate::error::{Error, Result};
use std::fs;
use std::path::Path;

/// Check if a device is currently mounted
///
/// On Linux, this parses /proc/mounts to check if the device is mounted.
pub fn check_not_mounted(device_path: impl AsRef<Path>) -> Result<()> {
    let device_path = resolve_device_path(device_path.as_ref())?;

    // Read /proc/mounts
    let mounts = fs::read_to_string("/proc/mounts").map_err(|e| {
        Error::Io(std::io::Error::other(format!(
            "Failed to read /proc/mounts: {}",
            e
        )))
    })?;

    // Check each mount entry
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let mount_device = parts[0];
            let mount_point = parts[1];

            // Check if this mount entry matches our device
            if let Ok(resolved_mount) = resolve_device_path(Path::new(mount_device)) {
                if resolved_mount == device_path {
                    return Err(Error::DeviceMounted(device_path, mount_point.to_string()));
                }
            }
        }
    }

    Ok(())
}

/// Resolve a device path to its canonical form
///
/// This handles symlinks (e.g., /dev/disk/by-uuid/... -> /dev/sda1)
fn resolve_device_path(path: &Path) -> Result<String> {
    // Try to canonicalize the path
    match path.canonicalize() {
        Ok(canonical) => Ok(canonical.to_string_lossy().to_string()),
        Err(_) => {
            // If canonicalize fails (e.g., path doesn't exist), just return the original
            Ok(path.to_string_lossy().to_string())
        }
    }
}

/// Check if running as root (required for block device access)
pub fn check_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Get the size of a block device in bytes
#[cfg(target_os = "linux")]
pub fn get_block_device_size(path: impl AsRef<Path>) -> Result<u64> {
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    let path = path.as_ref();
    let file = File::open(path).map_err(|_| Error::DeviceNotFound(path.display().to_string()))?;
    let fd = file.as_raw_fd();

    // Use BLKGETSIZE64 ioctl
    let mut size: u64 = 0;

    // BLKGETSIZE64 = 0x80081272
    // Cast to Ioctl type (i32 on musl, u64 on glibc)
    #[allow(overflowing_literals)]
    const BLKGETSIZE64: libc::Ioctl = 0x80081272u32 as libc::Ioctl;

    let result = unsafe { libc::ioctl(fd, BLKGETSIZE64, &mut size) };

    if result == -1 {
        // Fall back to seek method
        use std::io::{Seek, SeekFrom};
        let mut file = file;
        let size = file.seek(SeekFrom::End(0))?;
        Ok(size)
    } else {
        Ok(size)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn get_block_device_size(path: impl AsRef<Path>) -> Result<u64> {
    use std::fs::File;
    use std::io::{Seek, SeekFrom};

    let path = path.as_ref();
    let mut file =
        File::open(path).map_err(|_| Error::DeviceNotFound(path.display().to_string()))?;
    let size = file.seek(SeekFrom::End(0))?;
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_resolve_device_path() {
        // Test with a regular file
        let file = NamedTempFile::new().unwrap();

        let resolved = resolve_device_path(file.path()).unwrap();
        // Should be an absolute path
        assert!(resolved.starts_with('/'));
    }

    #[test]
    fn test_check_not_mounted_file() {
        // Create a temp file - should not be mounted
        let file = NamedTempFile::new().unwrap();

        // Should succeed since temp files aren't mounted
        let result = check_not_mounted(file.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_block_device_size_file() {
        let file = NamedTempFile::new().unwrap();
        // Write some data
        std::fs::write(file.path(), vec![0u8; 4096]).unwrap();

        let size = get_block_device_size(file.path()).unwrap();
        assert_eq!(size, 4096);
    }
}
