# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### New
- added a note to the readme why we are not shrinking
- added `ClusterId` and `SectorNum` newtype wrappers for improved type safety
- added `Debug` trait implementation for `Device`

### Changed
- `fat32expander resize` prints the device, old size, new size and size increase in its closing "Summary" instead of in a "Resize operation" block before the work starts, so the numbers are the ones the resize used. The hint to rerun with `--ignore-backup-mismatch` is now part of the error message itself.
- replaced `String` with `PathBuf` for device paths throughout the codebase
- public API functions now accept `impl AsRef<Path>` instead of `&str`
- `ResizeOptions` now uses builder pattern: `ResizeOptions::new(path).dry_run(true)`
- replaced glob re-exports (`pub use module::*`) with explicit exports in `fat32/mod.rs` and `resize/mod.rs`
- extracted duplicate logic in `Device::open()` and `open_readonly()` into shared `open_impl()` helper
- extracted `print_verbose_resize_info()` helper from `resize_fat32()`
- used `let-else` pattern for cleaner checkpoint parsing

### Fixed
- Resizing a filesystem whose FAT marked no cluster as in use, not even the root directory, enlarged the FAT without writing it. The two FAT copies then differed, and the first could mark a cluster as in use that held no data. Both copies are now written in full, as in every other resize that enlarges the FAT.
- After an interrupted resize, `fat32expander resize --dry-run` showed a fresh resize plan instead of the recovery the real run then performs. It now says "Dry run: an interrupted resize would be resumed from phase ..." and shows the sizes of that recovery. When the real run would refuse to recover, the dry run now refuses the same way, with exit code 9.
- `fat32expander version` could show the git hash and build time of an older commit when the program was rebuilt from a later commit on the same branch. It now shows the commit it was built from.
- The error for an unsupported sector size said "(expected 512 or 4096)", although filesystems with 1024- and 2048-byte sectors are supported too. It now lists all four sizes.
- If a resize that did not need to enlarge the FAT was interrupted in its last step, the next run failed with "Backup boot sector does not match primary boot sector" and exit code 8 and needed `--ignore-backup-mismatch`. Such a resize now leaves a recovery record as well, and a plain rerun finishes it.
- If a resize was interrupted while moving data and the device was then enlarged again before the rerun, the rerun did not find the record of the interrupted resize, started over, and could write wrong contents into files. A rerun now finds the record wherever the device ends, finishes the interrupted resize, and a further run uses the added space. If the device has shrunk since, the rerun refuses with "An interrupted resize left its checkpoint at sector ...", exit code 9, and changes nothing.
- An interrupted resize that had already marked the filesystem as being resized could not be resumed after the device was enlarged again; it failed with "Filesystem has been invalidated by an interrupted resize operation". It now resumes.
- A boot sector whose backup copy location pointed outside the reserved area (into the FAT or the data area) was reported as a backup mismatch, and the advised rerun with `--ignore-backup-mismatch` then wrote a boot sector over that FAT or data sector. Such a filesystem is now refused with "Invalid backup boot sector" and exit code 5 before anything is written. An FSInfo location outside the reserved area is refused the same way, with "Invalid FSInfo sector".
- When `fat32expander resize` refused to resume an interrupted resize because "Interrupted data shift left no progress record" or "Resize checkpoint geometry does not fit", it exited 1 ("unexpected internal failure"). Both now exit 9, the code for a filesystem that needs recovery, and the messages no longer start with "Relocation failed:" or "Calculation error:".
- When `/proc/mounts` could not be read, `fat32expander resize` exited 10, the code for a device I/O failure, although no device had been touched. It now exits with the new code 12, "Could not check whether the device is mounted", and the message names the device.
- A resize that was interrupted while moving data, and then resumed, could write the wrong contents into files on a filesystem with free space between used clusters. Progress is now recorded often enough that a resumed run always copies intact data.
- A leftover resize record from another filesystem, or from an earlier size of this one, could make `fat32expander resize` "resume" an operation that never ran, write a new size computed for the wrong volume, and exit 0. Such a record is now refused with "Resize checkpoint does not match this filesystem", exit code 9, and nothing is written.
- `fat32expander resize` on a device smaller than its filesystem (a truncated image or the wrong device) printed "Filesystem is already at maximum size for the device ... Nothing to do." and exited 0. It now fails with exit code 7, as documented. A full-size filesystem whose backup boot sector does not match now exits 8 instead of reporting "Nothing to do".
- After a resize was interrupted and run again, the free space shown by `fat32expander info` and the operating system could be too high, because the new clusters were counted twice. This happened when the rerun used `--ignore-backup-mismatch`, or resumed an interrupted resize late in its last step. The free-cluster count is now computed from the FAT on every run, which also replaces an "Unknown" or wrong count with the real value.

## 0.0.1 - 2025-12-01
### New
- initial release


