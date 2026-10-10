//! Exit codes, grouped by what a caller should *do* about the failure.
//!
//! Deliberately not one code per error variant: the exit code is an ABI, and
//! the error enum changes shape far too often to be one. Callers branch on
//! "can't open the device" or "needs recovery", not on which constructor ran.
//!
//! 2 is left free for clap's own usage errors, and everything stays below 125,
//! where the shell's own conventions (126, 127, 128+n) begin.

use fat32expander::Error;

pub const OK: u8 = 0;
pub const INTERNAL: u8 = 1;
pub const DEVICE_UNOPENABLE: u8 = 3;
pub const DEVICE_MOUNTED: u8 = 4;
pub const NOT_USABLE_FAT32: u8 = 5;
pub const DEVICE_TOO_SMALL: u8 = 7;
pub const BACKUP_MISMATCH: u8 = 8;
pub const NEEDS_RECOVERY: u8 = 9;
pub const IO_FAILURE: u8 = 10;
pub const MOUNT_STATE_UNKNOWN: u8 = 12;
pub const DEVICE_TOO_LARGE: u8 = 13;
// 6 was ALREADY_MAX_SIZE and was never returned: "nothing to do" exits
// 0 ([F-R31]), and [F-05] removed the last code path that could have
// produced it. Not reused, for the same reason as 11 below.
// 11 was ROOT_REQUIRED, removed with the privilege gate in [F-R46]. A
// caller who cannot write the device now gets DEVICE_UNOPENABLE, which is
// what the kernel actually reported. The number is not reused: a script
// branching on 11 should find nothing rather than a new meaning.

/// Map a failure to its exit code. Anything that is not one of our own errors
/// -- a context-only `anyhow` error, say -- is an internal failure.
pub fn exit_code_for(err: &anyhow::Error) -> u8 {
    let Some(e) = err.downcast_ref::<Error>() else {
        return INTERNAL;
    };

    match e {
        Error::DeviceOpen { .. } => DEVICE_UNOPENABLE,
        Error::DeviceMounted(..) | Error::DeviceBusy(_) => DEVICE_MOUNTED,
        // The mount check could not look; no device was touched ([F-06]).
        Error::MountTableUnreadable { .. } => MOUNT_STATE_UNKNOWN,

        Error::InvalidFAT32(_)
        | Error::BootSectorValidation(_)
        | Error::FSInfoValidation(_)
        | Error::UnsupportedSectorSize(_) => NOT_USABLE_FAT32,

        // Nothing to do is success; `main` prints why.
        Error::AlreadyMaxSize => OK,
        Error::DeviceTooSmall { .. } => DEVICE_TOO_SMALL,
        Error::TooManyClusters { .. } => DEVICE_TOO_LARGE,
        Error::BackupMismatch => BACKUP_MISMATCH,

        // Everything that means "this filesystem is mid-resize": either we can
        // still recover it, or we know we cannot and the user must intervene.
        Error::BootSectorInvalidated
        | Error::InvalidatedFilesystem
        | Error::CheckpointCorrupted
        | Error::ResizeSizeMismatch(_)
        | Error::CheckpointMismatch { .. }
        | Error::CheckpointVersionUnsupported(_)
        | Error::CheckpointVolumeMismatch { .. }
        | Error::CheckpointGeometryUnusable { .. }
        | Error::CheckpointOutsideDevice { .. }
        | Error::ShiftProgressMissing => NEEDS_RECOVERY,

        Error::DeviceIo { .. }
        | Error::DeviceSync { .. }
        | Error::Io(_)
        | Error::SectorLengthMismatch { .. } => IO_FAILURE,

        // An out-of-range sector means our own arithmetic was wrong, not that
        // the device misbehaved ([F-R28]).
        Error::SectorOutOfBounds { .. } => INTERNAL,

        Error::Calculation(_) | Error::Relocation(_) => INTERNAL,

        // `Error` is `#[non_exhaustive]` ([F-26]). A variant added without an
        // arm here fails `every_variant_is_listed` in `src/error.rs` first.
        _ => INTERNAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::exit;

    fn code(e: Error) -> u8 {
        exit_code_for(&anyhow::Error::new(e))
    }

    /// [F-06]: a resume the tool refuses is a filesystem that needs recovery,
    /// not an internal failure.
    #[test]
    fn recovery_refusals_exit_with_needs_recovery() {
        assert_eq!(code(Error::ShiftProgressMissing), exit::NEEDS_RECOVERY);
        assert_eq!(
            code(Error::CheckpointGeometryUnusable {
                new_total_sectors: 1000,
                new_fat_size: 900,
            }),
            exit::NEEDS_RECOVERY
        );
    }

    /// [F-16]: a record from a newer build, or of another volume, is a
    /// refused resume like any other.
    #[test]
    fn checkpoint_format_refusals_exit_with_needs_recovery() {
        assert_eq!(
            code(Error::CheckpointVersionUnsupported(3)),
            exit::NEEDS_RECOVERY
        );
        assert_eq!(
            code(Error::CheckpointVolumeMismatch { record: 1, boot: 2 }),
            exit::NEEDS_RECOVERY
        );
    }

    /// Safety net for [F-11]: however the mount guard learns that the device
    /// is in use, the refusal keeps its exit code.
    #[test]
    fn a_mounted_device_exits_4() {
        let e = Error::DeviceMounted("/dev/sdx1".to_string(), "/mnt".to_string());
        assert_eq!(code(e), exit::DEVICE_MOUNTED);
        assert_eq!(exit::DEVICE_MOUNTED, 4);
    }

    /// [F-11]: the kernel refusing an exclusive open is a device in use.
    #[test]
    fn a_busy_device_exits_like_a_mounted_one() {
        assert_eq!(
            code(Error::DeviceBusy("/dev/sdx1".to_string())),
            exit::DEVICE_MOUNTED
        );
    }

    /// [F-06]: failing to read the mount table touched no device, so it is
    /// not a device I/O failure.
    #[test]
    fn an_unreadable_mount_table_has_its_own_exit_code() {
        let e = Error::MountTableUnreadable {
            device: "/dev/sdx1".to_string(),
            source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        };
        assert_eq!(code(e), exit::MOUNT_STATE_UNKNOWN);
        assert_eq!(exit::MOUNT_STATE_UNKNOWN, 12);
    }

    /// Safety net for [F-26]: every error variant keeps its exit code. The
    /// match in `exit_code_for` is exhaustive today, so a new variant cannot
    /// compile without a code; marking `Error` `#[non_exhaustive]` would need a
    /// wildcard arm there, and this table is what notices a variant falling
    /// into it. Literal numbers, not the `exit` constants: the codes are an ABI.
    #[test]
    fn every_error_variant_keeps_its_exit_code() {
        let io = || std::io::Error::from(std::io::ErrorKind::Other);
        let s = || "x".to_string();
        let table: Vec<(Error, u8)> = vec![
            (
                Error::DeviceOpen {
                    path: s(),
                    source: io(),
                },
                3,
            ),
            (Error::DeviceMounted(s(), s()), 4),
            (Error::DeviceBusy(s()), 4),
            (Error::InvalidFAT32(s()), 5),
            (Error::BootSectorValidation(s()), 5),
            (Error::BootSectorInvalidated, 9),
            (Error::FSInfoValidation(s()), 5),
            (Error::BackupMismatch, 8),
            (
                Error::DeviceTooSmall {
                    current: 1,
                    minimum: 2,
                },
                7,
            ),
            (Error::AlreadyMaxSize, 0),
            (
                Error::TooManyClusters {
                    clusters: 2,
                    maximum: 1,
                },
                13,
            ),
            (Error::Io(io()), 10),
            (
                Error::DeviceIo {
                    op: "read",
                    path: s(),
                    offset: 0,
                    sector: 0,
                    len: 1,
                    source: io(),
                },
                10,
            ),
            (
                Error::DeviceSync {
                    path: s(),
                    source: io(),
                },
                10,
            ),
            (
                Error::SectorLengthMismatch {
                    path: s(),
                    expected: 512,
                    actual: 1,
                },
                10,
            ),
            (
                Error::SectorOutOfBounds {
                    path: s(),
                    sector: 9,
                    count: 1,
                    total_sectors: 8,
                },
                1,
            ),
            (Error::Calculation(s()), 1),
            (Error::Relocation(s()), 1),
            (Error::UnsupportedSectorSize(3), 5),
            (Error::CheckpointCorrupted, 9),
            (Error::InvalidatedFilesystem, 9),
            (Error::ResizeSizeMismatch(1), 9),
            (
                Error::CheckpointMismatch {
                    old_total_sectors: 1,
                    new_total_sectors: 2,
                    old_fat_size: 1,
                    new_fat_size: 2,
                    boot_total_sectors: 3,
                    boot_fat_size: 3,
                },
                9,
            ),
            (Error::CheckpointVersionUnsupported(3), 9),
            (Error::CheckpointVolumeMismatch { record: 1, boot: 2 }, 9),
            (
                Error::CheckpointOutsideDevice {
                    sector: 9,
                    device_sectors: 8,
                },
                9,
            ),
            (
                Error::CheckpointGeometryUnusable {
                    new_total_sectors: 1,
                    new_fat_size: 1,
                },
                9,
            ),
            (Error::ShiftProgressMissing, 9),
            (
                Error::MountTableUnreadable {
                    device: s(),
                    source: io(),
                },
                12,
            ),
        ];

        for (err, expected) in table {
            let name = format!("{err:?}");
            assert_eq!(code(err), expected, "exit code of {name}");
        }
        // Not one of our errors at all.
        assert_eq!(exit_code_for(&anyhow::anyhow!("context only")), 1);
    }
}
