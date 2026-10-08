use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Check if a device is currently mounted
///
/// On Linux, this parses /proc/mounts to check if the device is mounted.
///
/// An image file counts as mounted when a loop device backed by it is mounted
/// ([F-11]). This check names the mount point; what closes the race between
/// it and the open is the exclusive open of a block device in `Device`.
pub fn check_not_mounted(device_path: impl AsRef<Path>) -> Result<()> {
    check_against_mounts_with(
        device_path.as_ref(),
        fs::read_to_string("/proc/mounts"),
        &loop_backing_file,
    )
}

/// The file behind a loop device, from sysfs; `None` for anything that is not
/// an attached loop device.
fn loop_backing_file(device: &str) -> Option<PathBuf> {
    let name = Path::new(device).file_name()?.to_str()?;
    if !device.starts_with("/dev/loop") {
        return None;
    }
    let text = fs::read_to_string(format!("/sys/block/{name}/loop/backing_file")).ok()?;
    Some(PathBuf::from(text.trim_end_matches('\n')))
}

/// [`check_against_mounts_with`] for a machine without loop devices.
#[cfg(test)]
fn check_against_mounts(device_path: &Path, mounts: std::io::Result<String>) -> Result<()> {
    check_against_mounts_with(device_path, mounts, &|_| None)
}

/// The decision behind [`check_not_mounted`], given the result of reading the
/// mount table and a way to find the file behind a loop device, so tests can
/// hand it both.
fn check_against_mounts_with(
    device_path: &Path,
    mounts: std::io::Result<String>,
    loop_backing: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<()> {
    let device_path = resolve_device_path(device_path)?;

    let mounts = mounts.map_err(|source| Error::MountTableUnreadable {
        device: device_path.clone(),
        source,
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

            // An image mounted through a loop device ([F-11]).
            if let Some(backing) = loop_backing(mount_device) {
                if resolve_device_path(&backing)? == device_path {
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
    // SAFETY: `geteuid` takes no arguments, borrows nothing, and cannot fail --
    // POSIX specifies it as always successful, so there is no errno to race on
    // and no pointer for the kernel to retain. The only obligation is that it
    // be declared correctly, which is libc's.
    //
    // [F-R47]: the soundness here is self-evident, which is exactly why it went
    // unwritten. An `unsafe` block with no argument is indistinguishable at a
    // glance from one whose argument was never made, so the reviewer has to
    // re-derive it every time.
    unsafe { libc::geteuid() == 0 }
}

/// Get the size in bytes of an already-open block device.
///
/// [F-R48]: this is the canonical way to size a block device, and it used to
/// be dead -- `Device` sized everything with `seek(SeekFrom::End(0))` while
/// this, the correct path, was reachable only from its own unit test. It is
/// now what `Device` calls, so the `unsafe` block below sits on the execution
/// path rather than beside it.
///
/// Takes an open file rather than a path so the caller's single `open()` is
/// preserved: re-opening by path here would add a TOCTOU window ([F-R27]).
///
/// `seek` remains the fallback for anything `BLKGETSIZE64` refuses -- notably
/// regular files, where the ioctl does not apply.
#[cfg(target_os = "linux")]
pub fn block_device_size_of(file: &fs::File) -> Result<u64> {
    use std::os::unix::io::AsRawFd;

    // BLKGETSIZE64 = 0x80081272
    // Cast to Ioctl type (i32 on musl, u64 on glibc)
    #[allow(overflowing_literals)]
    const BLKGETSIZE64: libc::Ioctl = 0x80081272u32 as libc::Ioctl;

    let mut size: u64 = 0;

    // SAFETY: `fd` is borrowed from `file`, which outlives this call, so the
    // descriptor is open and valid for the duration of the ioctl. BLKGETSIZE64
    // is defined to write exactly one `u64` through its third argument, and
    // `&mut size` is a unique, aligned, initialised pointer to exactly that.
    // The kernel does not retain the pointer past the call. A non-block-device
    // fd makes the call fail with -1, which is handled below, not UB.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), BLKGETSIZE64, &mut size) };

    if result == -1 {
        seek_size(file)
    } else {
        Ok(size)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn block_device_size_of(file: &fs::File) -> Result<u64> {
    seek_size(file)
}

/// Size of an open file by seeking to its end, on a private duplicate of the
/// handle so the caller's file offset is left alone.
fn seek_size(file: &fs::File) -> Result<u64> {
    use std::io::{Seek, SeekFrom};

    let mut probe = file.try_clone()?;
    Ok(probe.seek(SeekFrom::End(0))?)
}

/// Get the size of a block device in bytes, by path.
pub fn get_block_device_size(path: impl AsRef<Path>) -> Result<u64> {
    let path = path.as_ref();
    let file = fs::File::open(path).map_err(|source| Error::DeviceOpen {
        path: path.display().to_string(),
        source,
    })?;
    block_device_size_of(&file)
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

    /// Safety net for [F-06]: the mount check, driven through a mount table
    /// the test controls.
    #[test]
    fn a_device_listed_in_the_mount_table_is_refused() {
        let file = NamedTempFile::new().unwrap();
        let table = format!("{} /mnt/stick vfat rw 0 0\n", file.path().display());

        match check_against_mounts(file.path(), Ok(table)) {
            Err(Error::DeviceMounted(device, mount_point)) => {
                assert_eq!(device, resolve_device_path(file.path()).unwrap());
                assert_eq!(mount_point, "/mnt/stick");
            }
            other => panic!("a mounted device must be refused, got {:?}", other),
        }
    }

    #[test]
    fn a_mount_table_entry_naming_the_device_through_a_symlink_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let device = dir.path().join("device");
        std::fs::write(&device, b"").unwrap();
        let link = dir.path().join("by-uuid");
        std::os::unix::fs::symlink(&device, &link).unwrap();
        let table = format!("{} /mnt/stick vfat rw 0 0\n", link.display());

        assert!(matches!(
            check_against_mounts(&device, Ok(table)),
            Err(Error::DeviceMounted(..))
        ));
    }

    #[test]
    fn a_device_absent_from_the_mount_table_passes() {
        let file = NamedTempFile::new().unwrap();
        let table = "/dev/sda1 / ext4 rw 0 0\nproc /proc proc rw 0 0\n\n".to_string();

        assert!(check_against_mounts(file.path(), Ok(table)).is_ok());
    }

    #[test]
    fn an_unreadable_mount_table_is_a_failure_that_names_it() {
        let file = NamedTempFile::new().unwrap();
        let unreadable = Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        let err = check_against_mounts(file.path(), unreadable)
            .expect_err("a mount check that could not look must not pass");
        assert!(
            err.to_string().contains("/proc/mounts"),
            "the failure must say what could not be read: {}",
            err
        );
    }

    /// [F-06]: an unreadable mount table is its own failure, not device I/O.
    #[test]
    fn an_unreadable_mount_table_is_reported_as_such() {
        let file = NamedTempFile::new().unwrap();
        let unreadable = Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        assert!(matches!(
            check_against_mounts(file.path(), unreadable),
            Err(Error::MountTableUnreadable { .. })
        ));
    }

    /// [F-11]: an image file mounted through a loop device appears in the
    /// mount table as `/dev/loopN`, never under its own path.
    #[test]
    fn an_image_mounted_through_a_loop_device_is_refused() {
        let file = NamedTempFile::new().unwrap();
        let table = "/dev/loop7 /mnt/image vfat rw 0 0\n".to_string();
        let backing = file.path().to_path_buf();

        match check_against_mounts_with(file.path(), Ok(table), &|dev: &str| {
            (dev == "/dev/loop7").then(|| backing.clone())
        }) {
            Err(Error::DeviceMounted(device, mount_point)) => {
                assert_eq!(device, resolve_device_path(file.path()).unwrap());
                assert_eq!(mount_point, "/mnt/image");
            }
            other => panic!("a loop-mounted image must be refused, got {:?}", other),
        }
    }

    #[test]
    fn a_loop_device_backed_by_another_file_passes() {
        let file = NamedTempFile::new().unwrap();
        let other = NamedTempFile::new().unwrap();
        let table = "/dev/loop7 /mnt/image vfat rw 0 0\n".to_string();
        let backing = other.path().to_path_buf();

        assert!(
            check_against_mounts_with(file.path(), Ok(table), &|_: &str| { Some(backing.clone()) })
                .is_ok()
        );
    }

    #[test]
    fn only_loop_devices_have_a_backing_file() {
        assert_eq!(loop_backing_file("/dev/sda1"), None);
        assert_eq!(loop_backing_file("proc"), None);
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
