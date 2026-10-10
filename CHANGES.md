# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### New
- Programs that use `fat32expander` as a library can follow a resize as it runs: `resize_fat32_with_events` reports each step, a resumed or ignored recovery record, and, with `verbose`, the layout and every cluster moved. The library no longer writes these messages to the terminal itself; the `fat32expander` command prints them as before.
- added a note to the readme why we are not shrinking
- added `Debug` trait implementation for `Device`

### Changed
- `fat32expander info` and `fat32expander resize` now label sizes "MiB" instead of "MB" ("Current size: 35840000 bytes (34.18 MiB)"). The numbers do not change: they were always mebibytes (1024 × 1024 bytes), and the exact byte count is still printed beside them.
- For programs that use `fat32expander` as a library: with `default-features = false` in their `Cargo.toml`, they no longer build `clap` and `anyhow`, which only the `fat32expander` command uses. The command is now behind the default `cli` feature; `cargo build` and `cargo install` work as before.
- For programs that use `fat32expander` as a library: `Error` and `ResizeResult` are marked `#[non_exhaustive]`, so a `match` on `Error` needs a `_` arm and a `ResizeResult` can no longer be built outside the library. In return, later versions can add error kinds and result fields without breaking such programs.
- For programs that use `fat32expander` as a library: `Device::write_bytes_at`, `Device::read_bytes_at`, `Device::size_bytes`, `BootSector::as_bytes_mut`, `BootSector::set_root_cluster`, `BootSector::fs_type`, `FSInfo::as_bytes_mut` and `FSInfo::set_next_free` are removed. `write_bytes_at` wrote anywhere on the device without the checks every other write goes through.
- For programs that use `fat32expander` as a library: `ResizeResult` no longer has a `calculation` field, and `SizeCalculation` is no longer exported. `ResizeResult` now has `old_total_sectors`, `new_total_sectors`, `old_fat_size` and `new_fat_size` directly. The removed "free clusters after resize" value was wrong: it was 0 after a resumed resize.
- `fat32expander resize` prints the device, old size, new size and size increase in its closing "Summary" instead of in a "Resize operation" block before the work starts, so the numbers are the ones the resize used. The hint to rerun with `--ignore-backup-mismatch` is now part of the error message itself.
- replaced `String` with `PathBuf` for device paths throughout the codebase
- public API functions now accept `impl AsRef<Path>` instead of `&str`
- `ResizeOptions` now uses builder pattern: `ResizeOptions::new(path).dry_run(true)`

### Fixed
- Growing a filesystem with small clusters onto a very large device (4 KiB clusters past about 1 TiB, 512-byte clusters past about 128 GiB) reported success, but left more clusters than FAT32 can number, and Linux then refused to mount it. `fat32expander resize` now stops with "FAT32 allows at most 268435445" and exit code 13 and changes nothing; `fat32expander info` and `resize` reject an existing filesystem with too many clusters as not a usable FAT32 filesystem (exit code 5).
- Growing a filesystem whose FAT was larger than `fat32expander` would have made it wrote a smaller FAT size into the boot sector, so every file and directory then pointed at the wrong data. The FAT now never shrinks.
- Growing a filesystem whose FAT was exactly full by only a few sectors stopped halfway with "sector ... out of bounds", and every rerun failed the same way. The resize now keeps the FAT that still covers the grown volume; when the volume cannot gain a cluster on the device, it reports "already at maximum size" and changes nothing.
- `fat32expander resize` on an image file that was mounted through a loop device (`/dev/loopN`) went ahead and changed the filesystem under the mount. It now refuses with "Device '...' is currently mounted at '...'" and exit code 4.
- A block device could be mounted, or resized by a second `fat32expander`, while a resize was running on it. The resize now holds the device exclusively; if the device is already mounted or held, it stops with "Device '...' is in use: it is mounted or held by another program" and exit code 4.
- When `fat32expander resize` failed, it printed only the error, so there was no record of what it had already changed on the device. It now lists the completed steps under "Operations performed before the failure:" on stderr before the error. The exit codes do not change.
- A recovery record left in the last sector of a device that was reformatted afterwards could be taken for an interrupted resize of the new filesystem when the sizes matched, and the resize then moved data that was never meant to move. The record now names its filesystem, and a record of another filesystem is ignored with "Ignoring a resize checkpoint at sector ...: it was left by another filesystem" and the resize runs normally.
- A recovery record written by a newer version of `fat32expander` was ignored, and the resize started over on a filesystem that was half resized. It is now refused with "...checkpoint in format version ..., which this build does not know" and exit code 9. Versions before this one cannot read the records this version writes, so finish an interrupted resize with the version that started it.
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
- A leftover resize record from another filesystem, or from an earlier size of this one, could make `fat32expander resize` "resume" an operation that never ran, write a new size computed for the wrong volume, and exit 0. Such a record is now ignored and the resize runs normally. If the filesystem is marked as being resized, a record that does not match is refused with "Resize checkpoint does not match this filesystem", exit code 9, and nothing is written.
- `fat32expander resize` on a device smaller than its filesystem (a truncated image or the wrong device) printed "Filesystem is already at maximum size for the device ... Nothing to do." and exited 0. It now fails with exit code 7, as documented. A full-size filesystem whose backup boot sector does not match now exits 8 instead of reporting "Nothing to do".
- After a resize was interrupted and run again, the free space shown by `fat32expander info` and the operating system could be too high, because the new clusters were counted twice. This happened when the rerun used `--ignore-backup-mismatch`, or resumed an interrupted resize late in its last step. The free-cluster count is now computed from the FAT on every run, which also replaces an "Unknown" or wrong count with the real value.

## 0.0.1 - 2025-12-01
### New
- initial release


