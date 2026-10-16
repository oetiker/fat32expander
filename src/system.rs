use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// One entry of the mount table: what is mounted, and where.
#[derive(Debug, PartialEq, Eq)]
struct MountEntry {
    device: String,
    mount_point: String,
}

/// The mount table as this system reports it, with the name an error uses
/// for it.
struct MountTable {
    name: &'static str,
    entries: std::io::Result<Vec<MountEntry>>,
}

/// Check if a device is currently mounted
///
/// The mount table comes from `/proc/mounts` on Linux, `/etc/mnttab` on
/// illumos and `getfsstat` on macOS. On any other system it cannot be read,
/// so the check fails rather than vouch for a device it could not look at.
///
/// A raw device counts as its block twin (`/dev/rdisk4s1` as `/dev/disk4s1`
/// on macOS, `/dev/rdsk/...` as `/dev/dsk/...` on illumos), because the
/// mount table names only the block device.
///
/// An image file counts as mounted when a loop device backed by it is mounted
/// ([F-11]). This check names the mount point; on Linux, what closes the race
/// between it and the open is the exclusive open of a block device in
/// `Device`.
pub fn check_not_mounted(device_path: impl AsRef<Path>) -> Result<()> {
    check_against_mounts_with(
        device_path.as_ref(),
        read_mount_table(),
        &loop_backing_file,
        block_twin,
    )
}

#[cfg(target_os = "linux")]
fn read_mount_table() -> MountTable {
    MountTable {
        name: "/proc/mounts",
        entries: fs::read_to_string("/proc/mounts").map(|text| parse_proc_mounts(&text)),
    }
}

#[cfg(target_os = "illumos")]
fn read_mount_table() -> MountTable {
    MountTable {
        name: "/etc/mnttab",
        entries: fs::read_to_string("/etc/mnttab").map(|text| parse_mnttab(&text)),
    }
}

#[cfg(target_os = "macos")]
fn read_mount_table() -> MountTable {
    MountTable {
        name: "the mount table (getfsstat)",
        entries: macos_mounts(),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "illumos", target_os = "macos")))]
fn read_mount_table() -> MountTable {
    MountTable {
        name: "the mount table",
        entries: Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this build cannot read the mount table of this system",
        )),
    }
}

/// The mounted filesystems, from `getfsstat`, which fills a buffer the caller
/// owns. `getmntinfo` would hand back a buffer that its next call overwrites,
/// which a library cannot promise to any thread of its caller.
#[cfg(target_os = "macos")]
fn macos_mounts() -> std::io::Result<Vec<MountEntry>> {
    use std::ffi::CStr;

    // SAFETY: a null buffer asks only for the number of mounted filesystems;
    // nothing is written.
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Room for a few mounts that appear between the two calls; getfsstat
    // never writes more than the buffer holds.
    let capacity = count as usize + 8;
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(capacity);
    let bytes = (capacity * std::mem::size_of::<libc::statfs>()) as libc::c_int;
    // SAFETY: `buf` has room for `capacity` entries and `bytes` says exactly
    // that, so getfsstat writes only inside the allocation. It returns how
    // many entries it wrote, and only those are made visible below.
    let written = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if written < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: getfsstat initialised the first `written` entries, and
    // `written` is at most `capacity`.
    unsafe { buf.set_len(written as usize) };

    let text = |field: &[libc::c_char]| {
        // SAFETY: the kernel terminates both name fields with a NUL inside
        // the fixed-size array.
        unsafe { CStr::from_ptr(field.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    Ok(buf
        .iter()
        .map(|fs| MountEntry {
            device: text(&fs.f_mntfromname),
            mount_point: text(&fs.f_mntonname),
        })
        .collect())
}

/// `/proc/mounts`: one mount per line, fields separated by whitespace.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_mounts(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(MountEntry {
                device: fields.next()?.to_string(),
                mount_point: fields.next()?.to_string(),
            })
        })
        .collect()
}

/// `/etc/mnttab` on illumos: one mount per line, fields separated by tabs.
#[cfg_attr(not(target_os = "illumos"), allow(dead_code))]
fn parse_mnttab(text: &str) -> Vec<MountEntry> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(MountEntry {
                device: fields.next()?.to_string(),
                mount_point: fields.next()?.to_string(),
            })
        })
        .collect()
}

/// The block device a raw device path stands for, on this system.
#[cfg(target_os = "macos")]
fn block_twin(path: &str) -> String {
    macos_block_twin(path)
}

#[cfg(target_os = "illumos")]
fn block_twin(path: &str) -> String {
    illumos_block_twin(path)
}

#[cfg(not(any(target_os = "macos", target_os = "illumos")))]
fn block_twin(path: &str) -> String {
    path.to_string()
}

/// macOS: `/dev/rdiskN...` is the raw twin of `/dev/diskN...`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_block_twin(path: &str) -> String {
    match path.strip_prefix("/dev/rdisk") {
        Some(rest) => format!("/dev/disk{rest}"),
        None => path.to_string(),
    }
}

/// illumos: `/dev/rdsk/...` is the raw twin of `/dev/dsk/...`, and both are
/// symlinks into `/devices`, where the raw node ends in `,raw`.
#[cfg_attr(not(target_os = "illumos"), allow(dead_code))]
fn illumos_block_twin(path: &str) -> String {
    let path = path.strip_suffix(",raw").unwrap_or(path);
    match path.strip_prefix("/dev/rdsk/") {
        Some(rest) => format!("/dev/dsk/{rest}"),
        None => path.to_string(),
    }
}

/// What a device path names, for comparing it with a mount table entry: the
/// block twin, with symlinks resolved, of the block twin. The second pass
/// catches a raw `/dev` symlink that resolves to a raw `/devices` node.
fn device_identity(path: &str, block_twin: fn(&str) -> String) -> Result<String> {
    let resolved = resolve_device_path(Path::new(&block_twin(path)))?;
    Ok(block_twin(&resolved))
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

/// [`check_against_mounts_with`] for a Linux machine without loop devices.
#[cfg(test)]
fn check_against_mounts(device_path: &Path, mounts: std::io::Result<String>) -> Result<()> {
    check_against_mounts_with(device_path, proc_mounts(mounts), &|_| None, |p| {
        p.to_string()
    })
}

/// A `/proc/mounts` table from its text, for tests.
#[cfg(test)]
fn proc_mounts(text: std::io::Result<String>) -> MountTable {
    MountTable {
        name: "/proc/mounts",
        entries: text.map(|t| parse_proc_mounts(&t)),
    }
}

/// The decision behind [`check_not_mounted`], given the mount table, a way
/// to find the file behind a loop device and the system's raw-to-block
/// mapping, so tests can hand it all three.
fn check_against_mounts_with(
    device_path: &Path,
    mounts: MountTable,
    loop_backing: &dyn Fn(&str) -> Option<PathBuf>,
    block_twin: fn(&str) -> String,
) -> Result<()> {
    let device_path = resolve_device_path(device_path)?;
    let identity = device_identity(&device_path, block_twin)?;

    let entries = mounts
        .entries
        .map_err(|source| Error::MountTableUnreadable {
            device: device_path.clone(),
            table: mounts.name,
            source,
        })?;

    for entry in entries {
        if device_identity(&entry.device, block_twin)? == identity {
            return Err(Error::DeviceMounted(device_path, entry.mount_point));
        }

        // An image mounted through a loop device ([F-11]).
        if let Some(backing) = loop_backing(&entry.device) {
            if resolve_device_path(&backing)? == device_path {
                return Err(Error::DeviceMounted(device_path, entry.mount_point));
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

/// Whether the process runs with an effective user ID of 0.
///
/// Informational only: nothing in this crate requires root ([F-R46]). Whether
/// a device can be written is the kernel's answer to the open, not this one.
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

/// macOS: a seek to the end of a disk device gives 0, so the size comes from
/// the disk's block count and block size.
#[cfg(target_os = "macos")]
pub fn block_device_size_of(file: &fs::File) -> Result<u64> {
    use std::os::unix::io::AsRawFd;

    // From <sys/disk.h>: _IOR('d', 24, uint32_t) and _IOR('d', 25, uint64_t).
    const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
    const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;

    let mut block_size: u32 = 0;
    let mut block_count: u64 = 0;

    // SAFETY: `fd` is borrowed from `file`, which outlives both calls. Each
    // ioctl writes exactly one value of the type its request encodes (a
    // `u32`, then a `u64`) through a unique, aligned, initialised pointer to
    // a variable of that type, and the kernel keeps neither pointer. A file
    // that is not a disk makes the call fail with -1, handled below.
    let ok = unsafe {
        libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut block_size) != -1
            && libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut block_count) != -1
    };

    match block_count.checked_mul(u64::from(block_size)) {
        Some(size) if ok => Ok(size),
        _ => seek_size(file),
    }
}

/// illumos: a seek to the end of a disk device does not give its size, so it
/// comes from the media information of the disk driver.
#[cfg(target_os = "illumos")]
pub fn block_device_size_of(file: &fs::File) -> Result<u64> {
    use std::os::unix::io::AsRawFd;

    // From <sys/dkio.h>: DKIOCGMEDIAINFO = DKIOC | 42, DKIOC = 0x04 << 8.
    const DKIOCGMEDIAINFO: libc::c_int = (0x04 << 8) | 42;

    /// `struct dk_minfo` from <sys/dkio.h>.
    #[repr(C)]
    struct DkMinfo {
        dki_media_type: libc::c_uint,
        dki_lbsize: libc::c_uint,
        dki_capacity: u64,
    }

    let mut info = DkMinfo {
        dki_media_type: 0,
        dki_lbsize: 0,
        dki_capacity: 0,
    };

    // SAFETY: `fd` is borrowed from `file`, which outlives the call.
    // DKIOCGMEDIAINFO writes one `struct dk_minfo` through its argument, and
    // `&mut info` is a unique, aligned, initialised pointer to a `#[repr(C)]`
    // copy of that struct; the kernel keeps no pointer. A file that is not a
    // disk makes the call fail with -1, handled below.
    let result = unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGMEDIAINFO, &mut info) };

    match info.dki_capacity.checked_mul(u64::from(info.dki_lbsize)) {
        Some(size) if result != -1 => Ok(size),
        _ => seek_size(file),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "illumos")))]
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

        match check_against_mounts_with(
            file.path(),
            proc_mounts(Ok(table)),
            &|dev: &str| (dev == "/dev/loop7").then(|| backing.clone()),
            |p| p.to_string(),
        ) {
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

        assert!(check_against_mounts_with(
            file.path(),
            proc_mounts(Ok(table)),
            &|_: &str| Some(backing.clone()),
            |p| p.to_string(),
        )
        .is_ok());
    }

    #[test]
    fn proc_mounts_fields_are_separated_by_whitespace() {
        let entries = parse_proc_mounts("/dev/sdb1 /mnt/stick vfat rw 0 0\n\nnone\n");
        assert_eq!(
            entries,
            vec![MountEntry {
                device: "/dev/sdb1".to_string(),
                mount_point: "/mnt/stick".to_string(),
            }]
        );
    }

    /// illumos separates the fields of `/etc/mnttab` with tabs, so a mount
    /// point may hold a space.
    #[test]
    fn mnttab_fields_are_separated_by_tabs() {
        let entries = parse_mnttab(
            "/dev/dsk/c1t0d0p1\t/mnt/my stick\tpcfs\trw\t1700000000\n\
             /proc\t/proc\tproc\trw\t1700000000\n\n",
        );
        assert_eq!(
            entries,
            vec![
                MountEntry {
                    device: "/dev/dsk/c1t0d0p1".to_string(),
                    mount_point: "/mnt/my stick".to_string(),
                },
                MountEntry {
                    device: "/proc".to_string(),
                    mount_point: "/proc".to_string(),
                },
            ]
        );
    }

    #[test]
    fn a_macos_raw_disk_names_its_block_twin() {
        assert_eq!(macos_block_twin("/dev/rdisk4s1"), "/dev/disk4s1");
        assert_eq!(macos_block_twin("/dev/rdisk4"), "/dev/disk4");
        assert_eq!(macos_block_twin("/dev/disk4s1"), "/dev/disk4s1");
        assert_eq!(macos_block_twin("/tmp/rdisk4"), "/tmp/rdisk4");
    }

    /// `/dev/rdsk` and `/dev/dsk` are symlinks into `/devices`, where the raw
    /// node carries a `,raw` suffix the block node lacks.
    #[test]
    fn an_illumos_raw_disk_names_its_block_twin() {
        assert_eq!(
            illumos_block_twin("/dev/rdsk/c1t0d0p1"),
            "/dev/dsk/c1t0d0p1"
        );
        assert_eq!(
            illumos_block_twin("/devices/pci@0,0/pci1af4,2@4/blkdev@0,0:q,raw"),
            "/devices/pci@0,0/pci1af4,2@4/blkdev@0,0:q"
        );
        assert_eq!(illumos_block_twin("/dev/dsk/c1t0d0p1"), "/dev/dsk/c1t0d0p1");
    }

    /// A raw device is the same disk as its mounted block twin.
    #[test]
    fn a_raw_device_whose_block_twin_is_mounted_is_refused() {
        let table = MountTable {
            name: "the mount table",
            entries: Ok(vec![MountEntry {
                device: "/dev/disk4s1".to_string(),
                mount_point: "/Volumes/STICK".to_string(),
            }]),
        };

        match check_against_mounts_with(
            Path::new("/dev/rdisk4s1"),
            table,
            &|_: &str| None,
            macos_block_twin,
        ) {
            Err(Error::DeviceMounted(device, mount_point)) => {
                assert_eq!(device, "/dev/rdisk4s1");
                assert_eq!(mount_point, "/Volumes/STICK");
            }
            other => panic!("a mounted raw twin must be refused, got {:?}", other),
        }
    }

    #[test]
    fn an_unreadable_mount_table_is_named_by_the_system_that_failed() {
        let table = MountTable {
            name: "/etc/mnttab",
            entries: Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        };

        let err = check_against_mounts_with(
            Path::new("/dev/rdsk/c1t0d0p1"),
            table,
            &|_: &str| None,
            illumos_block_twin,
        )
        .expect_err("a mount check that could not look must not pass");
        assert!(err.to_string().contains("/etc/mnttab"), "{}", err);
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
