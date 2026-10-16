use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Wrapper around a block device or image file for sector-based I/O
pub struct Device {
    file: File,
    path: PathBuf,
    sector_size: u32,
    total_sectors: u64,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("path", &self.path)
            .field("sector_size", &self.sector_size)
            .field("total_sectors", &self.total_sectors)
            .finish_non_exhaustive()
    }
}

/// Records device mutations so a test can assert durability ordering.
///
/// A missing sync barrier ([F-R17], [F-R18]) cannot be observed any other way:
/// `sync()` returns nothing, and the QEMU crash harness kills the process, after
/// which the page cache flushes everything regardless of what the code asked
/// for. Recording the sequence of writes and flushes makes the barrier itself
/// the thing under test.
///
/// The journal is thread-local, so tests recording concurrently do not see each
/// other's operations; `resize_fat32` is synchronous and runs entirely on its
/// caller's thread.
#[cfg(feature = "device-journal")]
pub mod journal {
    use std::cell::RefCell;

    /// One recorded device mutation.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Op {
        /// A write covering `count` sectors starting at `start`.
        Write { start: u64, count: u64 },
        /// A durability barrier -- everything written before this is on disk.
        Sync,
    }

    thread_local! {
        static JOURNAL: RefCell<Vec<Op>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(op: Op) {
        JOURNAL.with(|j| j.borrow_mut().push(op));
    }

    /// Begin recording, discarding anything this thread recorded earlier.
    pub fn reset() {
        JOURNAL.with(|j| j.borrow_mut().clear());
    }

    /// Everything this thread has recorded since the last [`reset`].
    pub fn recorded() -> Vec<Op> {
        JOURNAL.with(|j| j.borrow().clone())
    }
}

/// How many leading bytes are handed to a sector-size probe.
///
/// Enough to cover the largest sector size FAT32 permits, so a probe can read
/// a complete boot sector whatever size it turns out to declare.
pub const PROBE_BYTES: usize = 4096;

/// [F-11]: whether to open `path` with `O_EXCL`.
///
/// A writable open of a block device claims it exclusively: Linux refuses it
/// with `EBUSY` while the device is mounted or claimed by another exclusive
/// opener, and refuses a mount for as long as the claim is held. That closes
/// the window between the mount check and the open, and keeps a second resize
/// off the device. A read-only open stays shared, so `info` still works on a
/// mounted device, and a regular file never gets the flag, because Linux
/// defines `O_EXCL` without `O_CREAT` only for block devices.
///
/// Only Linux documents that meaning. POSIX leaves `O_EXCL` without
/// `O_CREAT` undefined, so the flag is not set on macOS and illumos, and the
/// mount check is the only guard there.
fn opens_exclusively(path: &Path, writable: bool) -> bool {
    cfg!(target_os = "linux")
        && writable
        && std::fs::metadata(path).is_ok_and(|m| m.file_type().is_block_device())
}

/// Whether the kernel refusing an open with `EBUSY` means the device is in
/// use. On Linux only the exclusive claim draws `EBUSY`. macOS refuses every
/// open of the block device of a mounted volume with it, read-only ones too:
/// the `macos` CI job saw `info` and `resize` on a mounted `/dev/disk7` stop
/// with "Cannot open device" and exit 3 instead of exit 4. illumos is taken
/// to behave like macOS; no test has run there.
fn busy_means_in_use(exclusive: bool, linux: bool) -> bool {
    exclusive || !linux
}

/// The error for a failed open; an open the kernel refused as busy, where
/// that means in use, is a device in use, not one that cannot be opened.
fn open_error(path: String, source: std::io::Error, busy_is_in_use: bool) -> Error {
    if busy_is_in_use && source.raw_os_error() == Some(libc::EBUSY) {
        Error::DeviceBusy(path)
    } else {
        Error::DeviceOpen { path, source }
    }
}

/// Flush a file or device, including the write cache of the drive.
#[cfg(not(target_os = "macos"))]
fn sync_file(file: &File) -> std::io::Result<()> {
    file.sync_all()
}

/// macOS: `sync_all` calls `fcntl(F_FULLFSYNC)`, which only filesystems
/// support; on a raw disk device it fails with `ENOTTY`, as the `macos` CI
/// job saw on `/dev/rdisk7`. A disk device is flushed with `fsync` for the
/// kernel's buffers and `DKIOCSYNCHRONIZECACHE` for the drive's write cache,
/// which is what `F_FULLFSYNC` does for a file.
#[cfg(target_os = "macos")]
fn sync_file(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;

    let kind = file.metadata()?.file_type();
    if !kind.is_block_device() && !kind.is_char_device() {
        return file.sync_all();
    }

    // From <sys/disk.h>: _IO('d', 22).
    const DKIOCSYNCHRONIZECACHE: libc::c_ulong = 0x2000_6416;

    // SAFETY: `fd` is borrowed from `file`, which outlives both calls. fsync
    // takes only the descriptor, and DKIOCSYNCHRONIZECACHE takes no argument,
    // so no memory is shared with the kernel.
    unsafe {
        if libc::fsync(file.as_raw_fd()) == -1
            || libc::ioctl(file.as_raw_fd(), DKIOCSYNCHRONIZECACHE) == -1
        {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

impl Device {
    /// Open a device and establish its geometry in a single step.
    ///
    /// [F-R23]: a `Device` used to open with a hardcoded 512-byte sector size
    /// and acquire the real one later, as a side effect of reading the boot
    /// sector ([F-R40]). Between those two moments every sector-addressed read
    /// and write was silently misaddressed on any filesystem that was not
    /// 512-byte-sectored, and nothing reported an error. There is now no such
    /// window: `sector_size` is determined before `Self` exists.
    ///
    /// The device layer sits below `fat32` and must not parse FAT structures,
    /// so the caller supplies the knowledge: `sector_size_from` receives up to
    /// [`PROBE_BYTES`] leading bytes and returns the sector size the format
    /// declares. The file is opened once -- the probe reads through the same
    /// handle the returned `Device` keeps.
    pub fn open_with<P, F>(path: P, writable: bool, sector_size_from: F) -> Result<Self>
    where
        P: AsRef<Path>,
        F: FnOnce(&[u8]) -> Result<u32>,
    {
        let path_buf = path.as_ref().to_path_buf();
        let path_display = path_buf.display().to_string();

        let exclusive = opens_exclusively(&path_buf, writable);
        let mut options = OpenOptions::new();
        options.read(true).write(writable);
        if exclusive {
            options.custom_flags(libc::O_EXCL);
        }
        let file = options.open(&path_buf).map_err(|source| {
            let in_use = busy_means_in_use(exclusive, cfg!(target_os = "linux"));
            open_error(path_display.clone(), source, in_use)
        })?;

        let size = Self::size_of(&file)?;

        // Probe with whatever the device actually holds: a device shorter than
        // PROBE_BYTES is not automatically invalid, but one too short to hold a
        // boot sector at all is the probe's business to reject.
        let probe_len = size.min(PROBE_BYTES as u64) as usize;
        let mut probe = vec![0u8; probe_len];
        file.read_exact_at(&mut probe, 0)
            .map_err(|source| Error::DeviceIo {
                op: "read",
                path: path_display,
                offset: 0,
                sector: 0,
                len: probe_len,
                source,
            })?;

        let sector_size = sector_size_from(&probe)?;
        if sector_size == 0 {
            return Err(Error::UnsupportedSectorSize(sector_size));
        }

        Ok(Self {
            file,
            path: path_buf,
            sector_size,
            total_sectors: size / sector_size as u64,
        })
    }

    /// Open for read/write with a sector size the caller already knows.
    pub fn open_with_sector_size<P: AsRef<Path>>(path: P, sector_size: u32) -> Result<Self> {
        Self::open_with(path, true, |_| Ok(sector_size))
    }

    /// Open read-only (for dry-run) with a sector size the caller already knows.
    pub fn open_readonly_with_sector_size<P: AsRef<Path>>(
        path: P,
        sector_size: u32,
    ) -> Result<Self> {
        Self::open_with(path, false, |_| Ok(sector_size))
    }

    /// Size in bytes of an open device, whether image file or block device.
    ///
    /// [F-R48]: block devices are sized with `BLKGETSIZE64` via
    /// `system::block_device_size_of`, which is the canonical interface and
    /// which this used to bypass in favour of `seek`. The fd is already open,
    /// so no second `open()` is needed to ask.
    fn size_of(file: &File) -> Result<u64> {
        let metadata = file.metadata()?;
        if metadata.is_file() {
            Ok(metadata.len())
        } else {
            crate::system::block_device_size_of(file)
        }
    }

    /// Get the device path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the sector size in bytes
    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// Get total number of sectors
    pub fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    /// Attach this device's identity to a failed I/O call.
    ///
    /// The path and sector size live here and nowhere else, so this is the only
    /// place that can turn "failed to fill whole buffer" into a statement about
    /// a particular sector of a particular device ([F-R30]). Sector-addressed
    /// callers pass the offset they already computed; the sector is derived so
    /// byte-addressed callers get one too.
    fn io_err(&self, op: &'static str, offset: u64, len: usize, source: std::io::Error) -> Error {
        Error::DeviceIo {
            op,
            path: self.path.display().to_string(),
            offset,
            sector: offset / self.sector_size as u64,
            len,
            source,
        }
    }

    /// Reject sector-addressed I/O that would fall outside the device.
    ///
    /// [F-R28]: `total_sectors` was carried around and never consulted. On an
    /// image file -- the mode every integration and QEMU test uses -- a write
    /// past the end silently extends the file rather than failing, so a bad
    /// sector computation grew the image instead of being caught. This is the
    /// check that turns that into an error.
    ///
    /// The addition is checked: `start_sector` is derived from on-disk values
    /// on some paths, and a wrapping sum would compare small and pass.
    fn check_bounds(&self, start_sector: u64, count: u64) -> Result<()> {
        let end = start_sector
            .checked_add(count)
            .ok_or_else(|| self.bounds_err(start_sector, count))?;
        if end > self.total_sectors {
            return Err(self.bounds_err(start_sector, count));
        }
        Ok(())
    }

    fn bounds_err(&self, sector: u64, count: u64) -> Error {
        Error::SectorOutOfBounds {
            path: self.path.display().to_string(),
            sector,
            count,
            total_sectors: self.total_sectors,
        }
    }

    /// Read sectors starting at the given sector number
    pub fn read_sectors(&self, start_sector: u64, count: u32) -> Result<Vec<u8>> {
        self.check_bounds(start_sector, count as u64)?;

        let offset = start_sector * self.sector_size as u64;
        let size = count as usize * self.sector_size as usize;
        let mut buffer = vec![0u8; size];

        self.file
            .read_exact_at(&mut buffer, offset)
            .map_err(|e| self.io_err("read", offset, size, e))?;
        Ok(buffer)
    }

    /// Read a single sector
    pub fn read_sector(&self, sector: u64) -> Result<Vec<u8>> {
        self.read_sectors(sector, 1)
    }

    /// Write sectors starting at the given sector number
    pub fn write_sectors(&self, start_sector: u64, data: &[u8]) -> Result<()> {
        // Round up: a trailing partial sector still occupies one.
        let sectors = data.len().div_ceil(self.sector_size as usize) as u64;
        self.check_bounds(start_sector, sectors)?;

        let offset = start_sector * self.sector_size as u64;
        self.file
            .write_all_at(data, offset)
            .map_err(|e| self.io_err("write", offset, data.len(), e))?;

        #[cfg(feature = "device-journal")]
        journal::record(journal::Op::Write {
            start: start_sector,
            count: sectors,
        });

        Ok(())
    }

    /// Write a single sector
    pub fn write_sector(&self, sector: u64, data: &[u8]) -> Result<()> {
        if data.len() != self.sector_size as usize {
            return Err(Error::SectorLengthMismatch {
                path: self.path.display().to_string(),
                expected: self.sector_size,
                actual: data.len(),
            });
        }
        self.write_sectors(sector, data)
    }

    /// Flush all writes to disk
    pub fn sync(&self) -> Result<()> {
        sync_file(&self.file).map_err(|source| Error::DeviceSync {
            path: self.path.display().to_string(),
            source,
        })?;

        #[cfg(feature = "device-journal")]
        journal::record(journal::Op::Sync);

        Ok(())
    }

    /// Read raw bytes from a byte offset, for tests that check what landed on
    /// disk. Not bounds-checked and not journaled, so test-only ([F-17]).
    #[cfg(test)]
    pub fn read_bytes_at(&self, offset: u64, size: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        self.file
            .read_exact_at(&mut buffer, offset)
            .map_err(|e| self.io_err("read", offset, size, e))?;
        Ok(buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_device_open_file() {
        let file = NamedTempFile::new().unwrap();
        // Write 1MB of zeros
        let zeros = vec![0u8; 1024 * 1024];
        std::fs::write(file.path(), &zeros).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        assert_eq!(device.sector_size(), 512);
        assert_eq!(device.total_sectors(), 2048); // 1MB / 512 = 2048 sectors
    }

    #[test]
    fn test_device_read_write() {
        let file = NamedTempFile::new().unwrap();
        let zeros = vec![0u8; 1024 * 1024];
        std::fs::write(file.path(), &zeros).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();

        // Write test pattern to sector 10
        let test_data = vec![0xAB; 512];
        device.write_sector(10, &test_data).unwrap();

        // Read it back
        let read_data = device.read_sector(10).unwrap();
        assert_eq!(read_data, test_data);

        // Verify other sectors are still zeros
        let sector0 = device.read_sector(0).unwrap();
        assert_eq!(sector0, vec![0u8; 512]);
    }

    /// Safety net for [F-R29]: the fix replaces the blanket `DeviceNotFound`
    /// with an errno-bearing error. Whatever the variant becomes, opening a
    /// path that is not there must fail, and the message must name the path --
    /// that is what tells a user which device the failure is about.
    #[test]
    fn opening_a_missing_path_fails_and_names_the_path() {
        let missing = "/nonexistent/fat32expander/safety/net/device.img";
        let err =
            Device::open_with_sector_size(missing, 512).expect_err("a missing path must not open");

        assert!(
            err.to_string().contains(missing),
            "the error must name the path it refers to, got: {err}"
        );
    }

    /// A directory fails to open for a different reason than a missing file
    /// (EISDIR, not ENOENT). Today both collapse into one variant; [F-R29]
    /// separates them. Either way it must be an error, and must name the path.
    #[test]
    fn opening_a_directory_fails_and_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().display().to_string();
        let err = Device::open_with_sector_size(dir.path(), 512)
            .expect_err("a directory must not open as a device");

        assert!(
            err.to_string().contains(&path),
            "the error must name the path it refers to, got: {err}"
        );
    }

    /// Safety net for [F-R30]: the fix attaches path and sector context to I/O
    /// errors. Reading past the end of the device must remain an error and
    /// must not start returning zeroed data.
    #[test]
    fn reading_past_the_end_of_the_device_fails() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 4 * 512]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        assert_eq!(device.total_sectors(), 4);

        // Sector 4 is one past the last valid sector (0..=3).
        assert!(
            device.read_sector(4).is_err(),
            "reading past the end of the device must fail, not return zeros"
        );
    }

    /// [F-R30]: a failed sector read must say *which* device and *which*
    /// sector. `Device` holds both, so a bare `io::Error` ("failed to fill
    /// whole buffer") is throwing away context the type already has.
    #[test]
    fn sector_io_failures_name_the_device_and_the_sector() {
        // The device must believe sector 4 exists, or [F-R28]'s bounds check
        // rejects the read before any I/O is attempted and there is no
        // io::Error to preserve. Opening at 8 sectors and then shortening the
        // file behind the device's back leaves sector 4 in bounds but backed
        // by nothing, which is a genuine I/O failure -- the case this test is
        // about.
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 8 * 512]).unwrap();
        let path = file.path().display().to_string();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        assert_eq!(device.total_sectors(), 8);
        std::fs::File::options()
            .write(true)
            .open(file.path())
            .unwrap()
            .set_len(4 * 512)
            .unwrap();

        let err = device
            .read_sector(4)
            .expect_err("reading past the end must fail");
        let msg = err.to_string();

        assert!(
            msg.contains(&path),
            "the error must name the device, got: {msg}"
        );
        assert!(
            msg.contains("sector 4"),
            "the error must name the sector, got: {msg}"
        );

        // The underlying io::Error must remain reachable, not be flattened
        // into a string.
        use std::error::Error as _;
        assert!(
            err.source()
                .and_then(|s| s.downcast_ref::<std::io::Error>())
                .is_some(),
            "the underlying io::Error must survive as the source, got: {err:?}"
        );
    }

    /// The bootstrap read of the boot sector is byte-addressed, and it is the
    /// case [F-R30]'s evidence actually cites -- a short read of the backup
    /// boot sector. It must carry the device too.
    #[test]
    fn byte_addressed_io_failures_name_the_device() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 512]).unwrap();
        let path = file.path().display().to_string();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        let err = device
            .read_bytes_at(0, 4096)
            .expect_err("reading more than the file holds must fail");

        assert!(
            err.to_string().contains(&path),
            "the error must name the device, got: {err}"
        );
    }

    /// The motivating case in [F-R29]: a file readable but not writable is the
    /// EACCES path a caller without write access hits, and it used to collapse
    /// into the same "not found" message as a missing device. The two existing
    /// open tests cover ENOENT and EISDIR only, so without this the errno the
    /// fix preserves has no witness on the one path the report actually cites.
    ///
    /// Whatever variant the fix introduces, a write-open of an unwritable file
    /// must fail and must name the path.
    #[test]
    fn opening_a_write_protected_file_for_writing_fails_and_names_the_path() {
        use std::os::unix::fs::PermissionsExt;

        // root ignores the permission bits entirely, so under root this asserts
        // nothing. Skip loudly rather than pass hollowly.
        if crate::system::check_root() {
            eprintln!("skipping: running as root, permission bits do not apply");
            return;
        }

        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 1024 * 1024]).unwrap();
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o444)).unwrap();
        let path = file.path().display().to_string();

        // Read-only access must still work -- the fix must not turn a
        // permission distinction into a blanket refusal.
        Device::open_readonly_with_sector_size(file.path(), 512)
            .expect("a readable file must open read-only");

        let err = Device::open_with_sector_size(file.path(), 512)
            .expect_err("a write-open of a read-only file must not succeed");
        assert!(
            err.to_string().contains(&path),
            "the error must name the path it refers to, got: {err}"
        );
    }

    /// [F-R29]: the errno must survive the open failure. A missing file and an
    /// unreadable one are different problems with different fixes, and
    /// collapsing both into "not found" sends the user looking for the wrong
    /// one.
    #[test]
    fn open_failures_preserve_the_underlying_errno() {
        use std::error::Error as _;
        use std::os::unix::fs::PermissionsExt;

        let missing =
            Device::open_with_sector_size("/nonexistent/fat32expander/errno/device.img", 512)
                .expect_err("a missing path must not open");
        let kind = missing
            .source()
            .and_then(|s| s.downcast_ref::<std::io::Error>())
            .map(|io| io.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::NotFound),
            "a missing device must carry ENOENT as its source, got {missing:?}"
        );

        if crate::system::check_root() {
            eprintln!("skipping the permission half: running as root");
            return;
        }

        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 1024]).unwrap();
        std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o444)).unwrap();

        let denied = Device::open_with_sector_size(file.path(), 512)
            .expect_err("a read-only file must not open for write");
        let kind = denied
            .source()
            .and_then(|s| s.downcast_ref::<std::io::Error>())
            .map(|io| io.kind());
        assert_eq!(
            kind,
            Some(std::io::ErrorKind::PermissionDenied),
            "a permission failure must carry EACCES as its source, got {denied:?}"
        );
    }

    // ===== Group A safety net: device geometry =====
    //
    // [F-R23] opens every `Device` with a hardcoded 512-byte sector size and
    // derives `total_sectors` from it; the real size arrives later, as a side
    // effect of reading the boot sector ([F-R40]). The fixes move that
    // determination to construction. What must survive, however the geometry
    // gets established, is the relationship between the three numbers: sector
    // size, total sectors, and the byte offset a sector index addresses.
    //
    // These tests assert that relationship, not the API that establishes it.
    // If `set_sector_size` disappears, they follow it to whatever replaces it;
    // the expected values do not move.

    /// The size in bytes of a device is fixed by the file behind it. Whatever
    /// sector size a `Device` ends up believing in, `total_sectors` must be
    /// that byte count divided by it -- otherwise the last sector, where the
    /// checkpoint lives, is not where anyone thinks it is.
    #[test]
    fn total_sectors_always_matches_the_device_size_divided_by_the_sector_size() {
        let file = NamedTempFile::new().unwrap();
        let len = 1024 * 1024u64;
        std::fs::write(file.path(), vec![0u8; len as usize]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        assert_eq!(
            device.total_sectors(),
            len / device.sector_size() as u64,
            "geometry must be self-consistent at open"
        );

        // The sector size now arrives at construction, so each size is a fresh
        // open rather than a mutation of an existing device.
        for size in [1024u32, 2048, 4096, 512] {
            let device = Device::open_with_sector_size(file.path(), size).unwrap();
            assert_eq!(device.sector_size(), size);
            assert_eq!(
                device.total_sectors(),
                len / size as u64,
                "total_sectors must be re-derived for a {size}-byte sector"
            );
        }
    }

    /// [F-R23]'s concrete consequence: a sector index means a byte offset, and
    /// the multiplier is the sector size. If a `Device` addresses sectors with
    /// one sector size while the filesystem was written with another, every
    /// read and write silently lands in the wrong place.
    #[test]
    fn a_sector_index_addresses_sector_size_times_index_bytes() {
        let file = NamedTempFile::new().unwrap();
        let len = 64 * 4096usize;
        std::fs::write(file.path(), vec![0u8; len]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 4096).unwrap();

        let marker = vec![0xC7u8; 4096];
        device.write_sector(3, &marker).unwrap();

        // Read the same bytes back byte-addressed: sector 3 of a 4096-byte
        // sector device is bytes 12288..16384, not 1536..2048.
        assert_eq!(
            device.read_bytes_at(3 * 4096, 4096).unwrap(),
            marker,
            "sector 3 must live at byte 12288 on a 4096-byte-sector device"
        );
        assert_eq!(
            device.read_bytes_at(2 * 4096, 4096).unwrap(),
            vec![0u8; 4096],
            "the preceding sector must be untouched"
        );
        assert_eq!(device.read_sector(3).unwrap(), marker);
    }

    /// The checkpoint is written to `total_sectors() - 1`. That write is
    /// legitimate and must stay legitimate: [F-R28] adds a bounds check to
    /// `read_sectors`/`write_sectors`, and an off-by-one there would break
    /// crash recovery on every resize. The last sector must remain both
    /// writable and readable, and writing it must not extend the device.
    #[test]
    fn the_last_sector_is_readable_writable_and_does_not_grow_the_device() {
        let file = NamedTempFile::new().unwrap();
        let len = 32 * 4096usize;
        std::fs::write(file.path(), vec![0u8; len]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 4096).unwrap();

        let last = device.total_sectors() - 1;
        assert_eq!(last, 31);

        let marker = vec![0x5Au8; 4096];
        device
            .write_sector(last, &marker)
            .expect("the last sector must be writable");
        assert_eq!(
            device.read_sector(last).unwrap(),
            marker,
            "the last sector must read back what was written"
        );

        assert_eq!(
            std::fs::metadata(file.path()).unwrap().len() as usize,
            len,
            "writing within the device must not extend it"
        );
        assert_eq!(
            device.total_sectors(),
            32,
            "the geometry must not have moved"
        );
    }

    /// [F-R48]: `get_block_device_size` is the only correct way to size a block
    /// device, and it is dead -- `open_impl` uses `seek(End)` instead. Wiring
    /// the ioctl in must not change the answer for the case both paths already
    /// handle. A regular file is the only device type this suite can create, so
    /// it is where the two must be pinned to agree.
    #[test]
    fn device_open_and_get_block_device_size_agree_on_the_same_path() {
        let file = NamedTempFile::new().unwrap();
        let len = 1024 * 1024u64 + 4096;
        std::fs::write(file.path(), vec![0u8; len as usize]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        let via_device = Device::size_of(&device.file).unwrap();
        let via_system = crate::system::get_block_device_size(file.path()).unwrap();

        assert_eq!(via_device, len, "Device must report the true byte size");
        assert_eq!(
            via_system, via_device,
            "the two sizing paths must not disagree"
        );
    }

    /// [F-R28]: a write past the end of an image file used to silently extend
    /// it, so a bad sector computation grew the device instead of failing.
    #[test]
    fn writing_past_the_end_of_the_device_is_rejected_and_does_not_extend_it() {
        let file = NamedTempFile::new().unwrap();
        let len = 4 * 512usize;
        std::fs::write(file.path(), vec![0u8; len]).unwrap();
        let path = file.path().display().to_string();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();
        assert_eq!(device.total_sectors(), 4);

        // Sector 4 is one past the last valid sector (0..=3).
        let err = device
            .write_sector(4, &vec![0xAB; 512])
            .expect_err("a write past the end must be rejected");
        let msg = err.to_string();
        assert!(msg.contains(&path), "the error must name the device: {msg}");
        assert!(
            msg.contains('4'),
            "the error must name the offending sector: {msg}"
        );

        assert_eq!(
            std::fs::metadata(file.path()).unwrap().len() as usize,
            len,
            "the rejected write must not have extended the device"
        );
    }

    /// A multi-sector write straddling the end must be rejected whole -- a
    /// guard that only checked the starting sector would let it through and
    /// extend the device by the overhang.
    #[test]
    fn a_multi_sector_write_straddling_the_end_is_rejected() {
        let file = NamedTempFile::new().unwrap();
        let len = 4 * 512usize;
        std::fs::write(file.path(), vec![0u8; len]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();

        // Starts inside (sector 3) but runs two sectors past the end.
        assert!(
            device.write_sectors(3, &vec![0xAB; 3 * 512]).is_err(),
            "a write straddling the end must be rejected"
        );
        assert_eq!(
            std::fs::metadata(file.path()).unwrap().len() as usize,
            len,
            "the rejected write must not have extended the device"
        );

        // The same read must be refused rather than reported as a short read.
        assert!(
            device.read_sectors(3, 3).is_err(),
            "a read straddling the end must be rejected"
        );

        // ...while a run that exactly reaches the end is still allowed.
        device
            .write_sectors(2, &vec![0xCD; 2 * 512])
            .expect("a write ending exactly at the last sector must be allowed");
        assert_eq!(device.read_sectors(2, 2).unwrap(), vec![0xCD; 2 * 512]);
    }

    /// The bounds arithmetic must not wrap: a huge starting sector plus a count
    /// can sum to something small, which an unchecked comparison would accept.
    #[test]
    fn a_wrapping_sector_range_is_rejected() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 4 * 512]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();

        assert!(
            device.read_sectors(u64::MAX, 4).is_err(),
            "a wrapping range must be rejected, not wrapped into bounds"
        );
        assert!(
            device
                .write_sectors(u64::MAX - 1, &vec![0xAB; 4 * 512])
                .is_err(),
            "a wrapping range must be rejected, not wrapped into bounds"
        );
    }

    /// The sector-length guard is the only bounds check the device layer has
    /// today. [F-R30] rewraps this error; it must keep rejecting the write.
    #[test]
    fn writing_a_sector_of_the_wrong_length_is_rejected() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 1024 * 1024]).unwrap();

        let device = Device::open_with_sector_size(file.path(), 512).unwrap();

        assert!(
            device.write_sector(0, &vec![0xAB; 511]).is_err(),
            "a short buffer must be rejected"
        );
        assert!(
            device.write_sector(0, &vec![0xAB; 513]).is_err(),
            "an over-long buffer must be rejected"
        );

        // The rejected writes must not have modified the device.
        assert_eq!(device.read_sector(0).unwrap(), vec![0u8; 512]);
    }

    /// [F-11]: a writable open of a block device claims it with `O_EXCL`, so
    /// the kernel refuses it while the device is mounted, and refuses a mount
    /// or a second resize for as long as this one holds it. A regular file
    /// gets no `O_EXCL`, which Linux does not define without `O_CREAT`.
    #[test]
    fn only_a_writable_open_of_a_block_device_is_exclusive() {
        let file = NamedTempFile::new().unwrap();
        assert!(!opens_exclusively(file.path(), true));
        assert!(!opens_exclusively(file.path(), false));

        // Any block device on this machine; reading its type needs no access.
        let block = std::fs::read_dir("/sys/block")
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| std::path::Path::new("/dev").join(e.file_name()))
            .find(|p| {
                use std::os::unix::fs::FileTypeExt;
                std::fs::metadata(p).is_ok_and(|m| m.file_type().is_block_device())
            });
        if let Some(block) = block {
            assert!(opens_exclusively(&block, true), "{}", block.display());
            assert!(!opens_exclusively(&block, false), "{}", block.display());
        }
    }

    /// [F-11]: the kernel's refusal of an exclusive open says "in use", which
    /// is not the same as "cannot open".
    #[test]
    fn a_busy_exclusive_open_is_reported_as_in_use() {
        let busy = || std::io::Error::from_raw_os_error(libc::EBUSY);
        assert!(matches!(
            open_error("/dev/sdx1".to_string(), busy(), true),
            Error::DeviceBusy(path) if path == "/dev/sdx1"
        ));
        assert!(matches!(
            open_error("/dev/sdx1".to_string(), busy(), false),
            Error::DeviceOpen { .. }
        ));
    }

    /// On Linux only the exclusive claim draws `EBUSY`; macOS refuses even a
    /// read-only open of a mounted volume's block device with it.
    #[test]
    fn a_busy_open_means_in_use_off_linux_even_without_an_exclusive_claim() {
        assert!(busy_means_in_use(true, true));
        assert!(!busy_means_in_use(false, true));
        assert!(busy_means_in_use(true, false));
        assert!(busy_means_in_use(false, false));
    }
}
