use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::os::unix::fs::FileExt;
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

impl Device {
    /// Internal helper to open a device with specified mode
    fn open_impl<P: AsRef<Path>>(path: P, writable: bool) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let path_display = path_buf.display().to_string();

        let file = OpenOptions::new()
            .read(true)
            .write(writable)
            .open(&path_buf)
            .map_err(|source| Error::DeviceOpen {
                path: path_display,
                source,
            })?;

        // Get device size
        let metadata = file.metadata()?;
        let size = if metadata.is_file() {
            // Regular file (image)
            metadata.len()
        } else {
            // Block device - use seek to end to get size
            let mut f = file.try_clone()?;
            f.seek(SeekFrom::End(0))?
        };

        // Default to 512-byte sectors (most common)
        // We'll update this after reading the boot sector
        let sector_size = 512u32;
        let total_sectors = size / sector_size as u64;

        Ok(Self {
            file,
            path: path_buf,
            sector_size,
            total_sectors,
        })
    }

    /// Open a device or image file for read/write access
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_impl(path, true)
    }

    /// Open a device in read-only mode (for dry-run)
    pub fn open_readonly<P: AsRef<Path>>(path: P) -> Result<Self> {
        Self::open_impl(path, false)
    }

    /// Get the device path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the sector size in bytes
    pub fn sector_size(&self) -> u32 {
        self.sector_size
    }

    /// Update sector size (called after reading boot sector)
    pub fn set_sector_size(&mut self, size: u32) {
        self.sector_size = size;
        // Recalculate total sectors with new size
        if let Ok(file_size) = self.file_size() {
            self.total_sectors = file_size / size as u64;
        }
    }

    /// Get total number of sectors
    pub fn total_sectors(&self) -> u64 {
        self.total_sectors
    }

    /// Get total device size in bytes
    fn file_size(&self) -> Result<u64> {
        let metadata = self.file.metadata()?;
        if metadata.is_file() {
            Ok(metadata.len())
        } else {
            let mut f = self.file.try_clone()?;
            Ok(f.seek(SeekFrom::End(0))?)
        }
    }

    /// Read sectors starting at the given sector number
    pub fn read_sectors(&self, start_sector: u64, count: u32) -> Result<Vec<u8>> {
        let offset = start_sector * self.sector_size as u64;
        let size = count as usize * self.sector_size as usize;
        let mut buffer = vec![0u8; size];

        self.file.read_exact_at(&mut buffer, offset)?;
        Ok(buffer)
    }

    /// Read a single sector
    pub fn read_sector(&self, sector: u64) -> Result<Vec<u8>> {
        self.read_sectors(sector, 1)
    }

    /// Write sectors starting at the given sector number
    pub fn write_sectors(&self, start_sector: u64, data: &[u8]) -> Result<()> {
        let offset = start_sector * self.sector_size as u64;
        self.file.write_all_at(data, offset)?;
        Ok(())
    }

    /// Write a single sector
    pub fn write_sector(&self, sector: u64, data: &[u8]) -> Result<()> {
        if data.len() != self.sector_size as usize {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "Data size {} does not match sector size {}",
                    data.len(),
                    self.sector_size
                ),
            )));
        }
        self.write_sectors(sector, data)
    }

    /// Flush all writes to disk
    pub fn sync(&self) -> Result<()> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Read raw bytes from a byte offset (used for bootstrapping before sector size is known)
    pub fn read_bytes_at(&self, offset: u64, size: usize) -> Result<Vec<u8>> {
        let mut buffer = vec![0u8; size];
        self.file.read_exact_at(&mut buffer, offset)?;
        Ok(buffer)
    }

    /// Write raw bytes at a byte offset
    pub fn write_bytes_at(&self, offset: u64, data: &[u8]) -> Result<()> {
        self.file.write_all_at(data, offset)?;
        Ok(())
    }

    /// Get total device size in bytes (public version)
    pub fn size_bytes(&self) -> Result<u64> {
        self.file_size()
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

        let device = Device::open(file.path()).unwrap();
        assert_eq!(device.sector_size(), 512);
        assert_eq!(device.total_sectors(), 2048); // 1MB / 512 = 2048 sectors
    }

    #[test]
    fn test_device_read_write() {
        let file = NamedTempFile::new().unwrap();
        let zeros = vec![0u8; 1024 * 1024];
        std::fs::write(file.path(), &zeros).unwrap();

        let device = Device::open(file.path()).unwrap();

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
        let err = Device::open(missing).expect_err("a missing path must not open");

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
        let err = Device::open(dir.path()).expect_err("a directory must not open as a device");

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

        let device = Device::open(file.path()).unwrap();
        assert_eq!(device.total_sectors(), 4);

        // Sector 4 is one past the last valid sector (0..=3).
        assert!(
            device.read_sector(4).is_err(),
            "reading past the end of the device must fail, not return zeros"
        );
    }

    /// The motivating case in [F-R29]: a file readable but not writable is the
    /// EACCES path a non-root user hits after `--force`, and today it collapses
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
        Device::open_readonly(file.path()).expect("a readable file must open read-only");

        let err = Device::open(file.path())
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

        let missing = Device::open("/nonexistent/fat32expander/errno/device.img")
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

        let denied =
            Device::open(file.path()).expect_err("a read-only file must not open for write");
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

    /// The sector-length guard is the only bounds check the device layer has
    /// today. [F-R30] rewraps this error; it must keep rejecting the write.
    #[test]
    fn writing_a_sector_of_the_wrong_length_is_rejected() {
        let file = NamedTempFile::new().unwrap();
        std::fs::write(file.path(), vec![0u8; 1024 * 1024]).unwrap();

        let device = Device::open(file.path()).unwrap();

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
}
