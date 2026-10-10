use thiserror::Error;

/// All errors that can occur during FAT32 resize operations
///
/// `#[non_exhaustive]` ([F-26]): a new variant is not a breaking change for a
/// caller, who must match with a wildcard arm. Inside this crate the match
/// stays exhaustive; `every_variant_is_listed` makes a new variant a compile
/// error until it has an exit code.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// A device could not be opened. The underlying `io::Error` is kept as the
    /// source rather than discarded: "not found", "permission denied" and
    /// "busy" are different problems with different fixes, and a caller
    /// without write access hitting EACCES is the common case ([F-R29]).
    /// Since [F-R46] removed the privilege gate, this is also the *only*
    /// report such a caller gets, so the distinction carries real weight.
    ///
    /// The errno is deliberately not interpolated into the message -- callers
    /// render the source chain, and interpolating would print it twice.
    #[error("Cannot open device '{path}'")]
    DeviceOpen {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Device '{0}' is currently mounted at '{1}'")]
    DeviceMounted(String, String),

    /// [F-11]: the kernel refused the exclusive open of a block device. It is
    /// mounted, or another program -- a second resize, say -- holds it.
    #[error("Device '{0}' is in use: it is mounted or held by another program")]
    DeviceBusy(String),

    #[error("Not a valid FAT32 filesystem: {0}")]
    InvalidFAT32(String),

    #[error("Boot sector validation failed: {0}")]
    BootSectorValidation(String),

    /// The boot signature is zeroed -- this tool's own marker for "a resize is
    /// in progress", written across the unsafe window and cleared on commit.
    ///
    /// This means *recovery may be possible, try it*, and is what the CLI keys
    /// its recovery decision on. It is deliberately **not**
    /// [`Error::InvalidatedFilesystem`], which the executor constructs to mean
    /// the opposite -- recovery was attempted and could not proceed. Merging
    /// the two would restate [F-R5]'s conflation in the type system, where it
    /// would look correct.
    #[error("Boot sector is invalidated (signature 0x0000), indicating an interrupted resize")]
    BootSectorInvalidated,

    #[error("FSInfo sector validation failed: {0}")]
    FSInfoValidation(String),

    #[error(
        "Backup boot sector does not match primary boot sector. This could indicate \
         filesystem corruption. Use --ignore-backup-mismatch to trust the primary and rewrite \
         the backup from it."
    )]
    BackupMismatch,

    #[error(
        "Device is smaller than current filesystem ({current} sectors < {minimum} sectors needed)"
    )]
    DeviceTooSmall { current: u64, minimum: u64 },

    #[error("Filesystem is already at maximum size for this device")]
    AlreadyMaxSize,

    /// [F-T02]: the grown volume would have more clusters than FAT32 can
    /// number. Nothing was written.
    #[error(
        "Growing to the full device would give {clusters} clusters, but FAT32 \
         allows at most {maximum}; the filesystem was not changed. A volume \
         this large needs a larger cluster size"
    )]
    TooManyClusters { clusters: u32, maximum: u32 },

    // [F-R46] removed `RootRequired`. The tool no longer guesses at write
    // access from euid: it attempts the open and reports what the kernel says,
    // which is both correct for non-root callers who do have access and more
    // specific when access is genuinely denied (`DeviceOpen` carries the path
    // and the errno).
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// A read or write against a device failed. `Device` knows the path and the
    /// sector size, so it fills both in rather than letting a bare
    /// "failed to fill whole buffer" escape with no idea which device or which
    /// sector it refers to ([F-R30]).
    ///
    /// `offset` and `sector` both appear because byte-addressed calls -- the
    /// boot-sector bootstrap reads 4096 bytes at offset 0 before the sector
    /// size is known -- cannot name a sector themselves, but `Device` can
    /// always derive one.
    #[error(
        "Failed to {op} {len} byte(s) at offset {offset} (sector {sector}) of device '{path}'"
    )]
    DeviceIo {
        op: &'static str,
        path: String,
        offset: u64,
        sector: u64,
        len: usize,
        #[source]
        source: std::io::Error,
    },

    /// Flushing a device failed. Has no offset to report, unlike [`Error::DeviceIo`].
    #[error("Failed to flush device '{path}'")]
    DeviceSync {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// A caller passed a buffer whose length is not the sector size. This is an
    /// argument error, not an I/O failure; it used to be dressed as an
    /// `io::Error` with `ErrorKind::InvalidInput`, which is the same
    /// conflation [F-R30] flags one level down.
    #[error("Buffer of {actual} byte(s) does not match the {expected}-byte sector size of device '{path}'")]
    SectorLengthMismatch {
        path: String,
        expected: u32,
        actual: usize,
    },

    /// Sector-addressed I/O was asked to touch a sector the device does not
    /// have. [F-R28]: nothing used to check this, and on an image file a write
    /// past the end silently extends the file instead of failing -- which is
    /// how [F-R2]'s misaddressed writes stayed invisible through every test.
    ///
    /// Reaching here means the caller's sector arithmetic is wrong, so it is an
    /// internal fault rather than an I/O condition.
    #[error(
        "Sector {sector}+{count} is outside device '{path}', which has {total_sectors} sector(s)"
    )]
    SectorOutOfBounds {
        path: String,
        sector: u64,
        count: u64,
        total_sectors: u64,
    },

    #[error("Calculation error: {0}")]
    Calculation(String),

    #[error("Relocation failed: {0}")]
    Relocation(String),

    #[error(
        "Device sector size {0} is not supported (expected one of {sizes:?})",
        sizes = crate::fat32::BootSector::VALID_SECTOR_SIZES
    )]
    UnsupportedSectorSize(u32),

    #[error("Resize checkpoint is corrupted (CRC mismatch)")]
    CheckpointCorrupted,

    #[error("Filesystem has been invalidated by an interrupted resize operation. Checkpoint not found or corrupted - cannot recover automatically.")]
    InvalidatedFilesystem,

    #[error(
        "Incomplete resize detected at phase {0}, but device size changed. Cannot safely resume."
    )]
    ResizeSizeMismatch(u8),

    /// [F-02]: the record was not written by an interrupted resize of this
    /// filesystem. A real record always names the geometry the boot sector
    /// still carries, because the new geometry is written last.
    #[error(
        "Resize checkpoint does not match this filesystem: it records {old_total_sectors} -> \
         {new_total_sectors} sectors with a FAT of {old_fat_size} -> {new_fat_size} sectors, but \
         the boot sector says {boot_total_sectors} sectors with a FAT of {boot_fat_size} sectors. \
         Cannot safely resume."
    )]
    CheckpointMismatch {
        old_total_sectors: u32,
        new_total_sectors: u32,
        old_fat_size: u32,
        new_fat_size: u32,
        boot_total_sectors: u32,
        boot_fat_size: u32,
    },

    /// [F-16]: the record carries our magic but a format version this build
    /// does not know. Reading it as "no checkpoint" would replan a half-done
    /// resize from scratch.
    #[error(
        "An interrupted resize left a checkpoint in format version {0}, which this build does \
         not know. Finish the resize with the build that started it."
    )]
    CheckpointVersionUnsupported(u8),

    /// [F-16]: the record belongs to another volume, for example one that
    /// was on the device before it was reformatted.
    #[error(
        "An interrupted resize left a checkpoint for volume {record:08X}, but this filesystem is \
         volume {boot:08X}. Cannot safely resume."
    )]
    CheckpointVolumeMismatch { record: u32, boot: u32 },

    /// [F-03]: the device shrank past the record since the resize started.
    #[error(
        "An interrupted resize left its checkpoint at sector {sector}, but the device now has \
         only {device_sectors} sectors. Restore the device to its size at the time of the \
         crash to resume."
    )]
    CheckpointOutsideDevice { sector: u64, device_sectors: u64 },

    /// [F-06]: the record matches the boot sector, but the geometry it would
    /// commit cannot hold a filesystem.
    #[error(
        "Resize checkpoint geometry does not fit: {new_total_sectors} sectors with a FAT of \
         {new_fat_size} sectors. Cannot safely resume."
    )]
    CheckpointGeometryUnusable {
        new_total_sectors: u32,
        new_fat_size: u32,
    },

    /// [F-06]: the data shift was interrupted, but recorded no progress, so
    /// the point it reached is unknown.
    #[error(
        "Interrupted data shift left no progress record, so the point it reached is unknown; \
         resuming would copy already-shifted data over itself. The filesystem is intact but \
         must be recovered from backup or repaired by hand."
    )]
    ShiftProgressMissing,

    /// [F-06]: the mount check could not look, so it cannot vouch for the
    /// device. No device was touched.
    #[error("Cannot tell whether {device} is mounted: failed to read /proc/mounts: {source}")]
    MountTableUnreadable {
        device: String,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// [F-26]: `Error` is `#[non_exhaustive]`, so the `fat32expander` binary
    /// maps it to exit codes with a wildcard arm and the compiler no longer
    /// flags a new variant there. This match is exhaustive because it is inside
    /// the crate. When it stops compiling, give the new variant an exit code in
    /// `exit_code_for` in `src/cli/exit.rs` and a row in
    /// `every_error_variant_keeps_its_exit_code`, then list it here.
    #[allow(dead_code)]
    fn every_variant_is_listed(e: &Error) {
        match e {
            Error::DeviceOpen { .. }
            | Error::DeviceMounted(..)
            | Error::DeviceBusy(_)
            | Error::InvalidFAT32(_)
            | Error::BootSectorValidation(_)
            | Error::BootSectorInvalidated
            | Error::FSInfoValidation(_)
            | Error::BackupMismatch
            | Error::DeviceTooSmall { .. }
            | Error::AlreadyMaxSize
            | Error::TooManyClusters { .. }
            | Error::Io(_)
            | Error::DeviceIo { .. }
            | Error::DeviceSync { .. }
            | Error::SectorLengthMismatch { .. }
            | Error::SectorOutOfBounds { .. }
            | Error::Calculation(_)
            | Error::Relocation(_)
            | Error::UnsupportedSectorSize(_)
            | Error::CheckpointCorrupted
            | Error::InvalidatedFilesystem
            | Error::ResizeSizeMismatch(_)
            | Error::CheckpointMismatch { .. }
            | Error::CheckpointVersionUnsupported(_)
            | Error::CheckpointVolumeMismatch { .. }
            | Error::CheckpointOutsideDevice { .. }
            | Error::CheckpointGeometryUnusable { .. }
            | Error::ShiftProgressMissing
            | Error::MountTableUnreadable { .. } => {}
        }
    }

    /// [F-22]: the message listed two of the four sizes FAT32 permits.
    #[test]
    fn the_unsupported_sector_size_message_names_every_valid_size() {
        let message = Error::UnsupportedSectorSize(8192).to_string();
        for size in ["512", "1024", "2048", "4096"] {
            assert!(message.contains(size), "{size} missing from: {message}");
        }
    }
}
