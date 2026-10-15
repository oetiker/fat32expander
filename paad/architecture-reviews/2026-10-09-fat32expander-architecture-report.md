# Architecture Report — fat32expander

**Date:** 2026-10-09
**Commit:** abef542bce3e6a321443f4a8b2d3102314ffae6d
**Languages:** Rust (2021 edition); thiserror, libc, crc32fast; clap and anyhow behind the default `cli` feature; shell (test scripts, QEMU harness)
**Key directories:** `src/`, `src/fat32/`, `src/resize/`, `src/cli/`, `tests/`, `scripts/`, `scripts/qemu/`, `.github/workflows/`
**Scope:** full repository

## Repo Overview

`fat32expander` is a Linux CLI and library that grows a FAT32 filesystem in place after its partition was enlarged. When the FAT must grow, the data area moves forward by whole sectors (cluster numbers are preserved), the larger FAT is written, and the new geometry is committed. A CRC-protected checkpoint, located through a mark in FSInfo's reserved bytes, lets an interrupted run resume.

Size: 23 source files (about 7,100 lines including inline tests) and 12 integration-test binaries (about 4,900 lines), plus shell and QEMU harnesses. Modules: `device` (sector I/O, `O_EXCL`), `system` (mount check, ioctl), `fat32` (on-disk structures, validation, FAT I/O), `resize` (calculator, relocator, checkpoint, executor, events, options, fsinfo, fault injection), and the binary-only `cli/` modules. Dependency direction: error ← system ← device ← fat32 ← resize ← crate root ← cli, with one small upward edge from `error` to `fat32` (see S-T01).

This is the third report. All 29 flaws of `2026-10-02-fat32expander-architecture-report.md` are marked Fixed. **IDs in this report carry a `T` prefix** (`[F-T01]`, `[S-T01]`) so that they do not collide with `[F-nn]` (2026-10-02) and `[F-Rnn]` (2026-07-28), which the code cites.

## Strengths

### [S-T01] Two-level public API, private leaves, FAT-agnostic device layer
- **Category:** S1 Clear modular boundaries / S4 Dependency direction is stable
- **Impact:** High
- **Explanation:** Only the crate root and `fat32` are public namespaces; `device`, `error`, `resize` and `system` are private. The device layer gets the sector-size probe as a closure and never parses FAT, and the CLI imports root re-exports only. One small upward edge exists: `error` names `fat32::BootSector::VALID_SECTOR_SIZES`.
- **Evidence:** `src/lib.rs:44-67`, excerpt: "mod device; mod error; pub mod fat32; mod resize; mod system;"; `src/device.rs:111-115` (`Device::open_with`), excerpt: "F: FnOnce(&[u8]) -> Result<u32>"; `src/cli/resize.rs:4`; `src/error.rs:146`
- **Found by:** Structure & Boundaries, Coupling & Dependencies

### [S-T02] Phase order enforced by witness types
- **Category:** S13 Domain modeling strength
- **Impact:** High
- **Explanation:** `DataShifted` → `FatExtended` → `FatOnDisk` are moved from phase to phase, so a wrong order or a repeated phase does not compile. A resume can skip a phase's work but must still consume the previous witness.
- **Evidence:** `src/resize/executor.rs:46-115, 376-446`, excerpt: "fn recorded_by_an_earlier_run(boot: BootSector, _shifted: DataShifted) -> Self"
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Integration & Data

### [S-T03] Fsync barriers before every durable promise
- **Category:** S12 Resilience patterns
- **Impact:** High
- **Explanation:** The checkpoint and its FSInfo mark are synced when written. The relocator syncs a batch before the progress record advances, and the FAT is synced before `FatWritten`. Backup and FSInfo are synced before the primary boot sector, and the primary before the checkpoint is cleared.
- **Evidence:** `src/resize/checkpoint.rs:388-414` (`write_checkpoint`, `mark_checkpoint_slot`); `src/resize/relocator.rs:271-279`; `src/resize/executor.rs:573, 648, 663`
- **Found by:** Integration & Data

### [S-T04] Shift progress spaced by a proven-safe cluster distance
- **Category:** S12 Resilience patterns
- **Impact:** High
- **Explanation:** Progress records are at most `min(1024, shift_clusters)` clusters apart, so a resumed batch never reads a source that an unsynced write already overwrote. A negative-control test shows that restarting from the top corrupts data.
- **Evidence:** `src/resize/relocator.rs:214-216`, excerpt: "shift_clusters => PROGRESS_RECORD_EVERY.min(shift_clusters)"; test `restarting_an_interrupted_shift_from_the_top_corrupts_data` (`src/resize/relocator.rs:849`)
- **Found by:** Integration & Data

### [S-T05] On-disk input handled as untrusted
- **Category:** S10 Security built-in / S12 Resilience patterns
- **Impact:** High
- **Explanation:** Layout accessors use checked arithmetic and return `Option`, `Device::check_bounds` uses `checked_add`, and FSInfo and backup pointers must lie in the reserved area. The checkpoint carries magic, a version, CRCs per record, and a volume-and-geometry match. Gap: see F-T10.
- **Evidence:** `src/fat32/structs.rs:294-337`; `src/fat32/validation.rs:66-86`; `src/resize/checkpoint.rs:182-208, 295-332`; `src/resize/relocator.rs:113-118`
- **Found by:** Integration & Data, Security & Code Quality

### [S-T06] Exclusive open and least privilege
- **Category:** S10 Security built-in
- **Impact:** High
- **Explanation:** A writable open of a block device uses `O_EXCL`, so the kernel refuses it while the device is mounted or held, and EBUSY becomes `DeviceBusy` (exit 4). The tool does not require root, and the only two `unsafe` blocks carry SAFETY comments.
- **Evidence:** `src/device.rs:82-84, 118-123`, excerpt: "options.custom_flags(libc::O_EXCL)"; `src/system.rs:106, 139`
- **Found by:** Security & Code Quality

### [S-T07] Typed error taxonomy with a pinned exit-code ABI
- **Category:** S7 Robust error handling / S6 Consistent API contracts
- **Impact:** High
- **Explanation:** `Error` is `#[non_exhaustive]` with structured payloads; device errors carry path, offset and sector. `exit.rs` groups variants by what the caller should do, retired codes 6 and 11 are not reused, and two tests keep the mapping complete and pinned. README and CLAUDE.md agree with the table.
- **Evidence:** `src/error.rs:247` (`every_variant_is_listed`); `src/cli/exit.rs:156` (`every_error_variant_keeps_its_exit_code`); `src/device.rs:198-207` (`io_err`); `src/device.rs:88-94`, excerpt: "if exclusive && source.raw_os_error() == Some(libc::EBUSY)"
- **Found by:** Error Handling & Observability, Integration & Data

### [S-T08] Broad, deterministic test strategy guarded in CI
- **Category:** S11 Testability & coverage
- **Impact:** High
- **Explanation:** Proptests cover boot sector parsing, the checkpoint codec, the calculator and whole resizes. CI runs `--all-targets`, `--ignored`, both feature-gated suites, the shell script and QEMU, and `ci_workflow_test.rs` and `manifest_test.rs` fail the build if parts of this are dropped. No test depends on time or randomness outside proptest.
- **Evidence:** `.github/workflows/tests.yml:69, 89, 108, 129`; `tests/ci_workflow_test.rs`; `tests/manifest_test.rs`; `tests/property_test.rs`
- **Found by:** Security & Code Quality

### [S-T09] Narrow test seams; test machinery compiles out
- **Category:** S11 Testability & coverage / S3 Loose coupling
- **Impact:** Medium
- **Explanation:** Each seam injects one effect: the mount table and loop resolver, the progress-record closure, and the device journal for sync order. Fault injection and the journal are feature-gated, and every `CrashPoint` has an end-to-end crash-then-resume test.
- **Evidence:** `src/system.rs:40-44` (`check_against_mounts_with`); `src/resize/relocator.rs:186-198` (`ShiftProgress`); `src/resize/fault.rs:68-85`, excerpt: "#[inline(always)] pub(crate) fn maybe_crash_at(_point: CrashPoint) {}"; `tests/crash_recovery_test.rs:347-403`
- **Found by:** Coupling & Dependencies, Security & Code Quality

### [S-T10] The library reports through events and never prints
- **Category:** S8 Observability present / S3 Loose coupling
- **Impact:** Medium
- **Explanation:** Progress reaches a caller-supplied callback as `ResizeEvent`s while the run goes on, so a failed run still shows how far it got. A test asserts the library writes nothing to stderr; the CLI owns the stream rule.
- **Evidence:** `src/resize/events.rs:67-108` (`Reporter`); `src/resize/executor.rs:148-152`; `tests/resize_events_test.rs:152` (`the_library_writes_nothing_to_stderr`)
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Error Handling & Observability

### [S-T11] Commit rerun converges
- **Category:** S12 Resilience patterns
- **Impact:** Medium
- **Explanation:** The FSInfo free count is derived from the FAT, not added to the on-disk hint, so a repeated commit writes the same value. The no-growth path writes a `FatWritten` checkpoint before it commits.
- **Evidence:** `src/resize/executor.rs:686-705` (`free_clusters_after_resize`), `src/resize/executor.rs:607-612`
- **Found by:** Integration & Data

### [S-T12] Versioned checkpoint format with named offsets
- **Category:** S6 Consistent API contracts
- **Impact:** Medium
- **Explanation:** The record has a version byte and named offsets, and later records are appended so the version-1 body stays byte-identical. A test pins the byte layout. Leftover literals: see F-T22.
- **Evidence:** `src/resize/checkpoint.rs:20-63`; test `checkpoint_writes_the_documented_byte_layout` (`src/resize/checkpoint.rs:520`)
- **Found by:** Integration & Data

### [S-T13] Cohesive modules and pragmatic abstractions
- **Category:** S2 High cohesion / S14 Simple, pragmatic abstractions
- **Impact:** Medium
- **Explanation:** Each `resize` submodule has one job; `checkpoint.rs` holds about 460 production lines, all about the record. `FSInfoReport` holds the `BootSector` rather than copying its fields, and no trait or generic exists without a second user.
- **Evidence:** `src/resize/checkpoint.rs:1-6`, excerpt: "everything about what a checkpoint *is* lives here"; `src/resize/fsinfo.rs:62-80` (`boot: BootSector`)
- **Found by:** Structure & Boundaries

### [S-T14] Lean dependencies
- **Category:** S5 Dependency management hygiene
- **Impact:** Medium
- **Explanation:** Runtime dependencies are thiserror, libc and crc32fast; clap and anyhow are optional behind `cli`. A manifest test and a `--no-default-features` CI check keep the library off CLI dependencies. Exception: F-T25.
- **Evidence:** `Cargo.toml:14`, excerpt: "cli = [\"dep:clap\", \"dep:anyhow\"]"; `.github/workflows/tests.yml:56`; `tests/manifest_test.rs`
- **Found by:** Coupling & Dependencies

### [S-T15] Configuration discipline
- **Category:** S9 Configuration discipline
- **Impact:** Low
- **Explanation:** All configuration is three CLI flags that map to the `ResizeOptions` builder, all off by default; `--ignore-backup-mismatch` is a narrow override, not a general force. The only environment variable, `FAT32_CRASH_AT`, exists only under `fault-injection`.
- **Evidence:** `src/resize/options.rs:57-70`; `src/resize/fault.rs:71-77`
- **Found by:** Error Handling & Observability

## Flaws/Risks

### [F-T01] A shift shorter than one cluster cannot be resumed
- **Category:** 19 Lack of idempotency (also 26)
- **Impact:** High
- **Explanation:** When `num_fats × FAT growth < sectors_per_cluster`, no progress record is written, so any crash in phase 0 ends every rerun in `ShiftProgressMissing` (exit 9), even a crash before the first move. Phase 0 moves every used cluster, so it is the longest phase, and the geometry is common with large clusters and modest growth. The error text says "The filesystem is intact" although after a mid-shift crash neither layout is; README promises resume without qualification, and the crash suites never use this geometry.
- **Evidence:** `src/resize/executor.rs:462-504` (`shift_cluster_data`), excerpt: "let resumable = plan.shift_clusters(boot) > 0;"; `src/error.rs:217-221`; README.md:157-166; test `execute_relocation_handles_a_shift_shorter_than_one_cluster` (`src/resize/relocator.rs:985-1000`) proves the geometry is reachable
- **Found by:** Integration & Data
- **Status:** Fixed
- **Status reason:** The calculator never plans a shift shorter than one cluster: when `num_fats × FAT growth < sectors_per_cluster` it grows the FAT to `old + ceil(spc / num_fats)` (costs less than one cluster; `AlreadyMaxSize` if the volume would lose clusters). The data shift records progress unconditionally and refuses a sub-cluster plan with moves before its first write. `ShiftProgressMissing` no longer claims the filesystem is intact; TECHGUIDE and CHANGES updated. New tests: proptest `a_growing_fat_shifts_the_data_by_at_least_one_cluster`, `a_shift_shorter_than_one_cluster_is_refused_before_any_write`, crash tests `a_crash_at_the_start_of_a_short_fat_growth_resumes` and `a_crash_in_the_middle_of_a_short_fat_growth_resumes`. With the user's approval the two crash tests that asserted the flaw (`an_interrupted_sub_cluster_shift_exits_9`, `..._refuses_to_resume`) were deleted and the fixture guard of `a_shift_shorter_than_one_cluster_keeps_every_file` was inverted.
- **Status date:** 2026-10-12 09:33 UTC
- **Status commit:** 8089141

### [F-T02] No upper bound on the FAT32 cluster count
- **Category:** 30 Security as an afterthought (untrusted geometry not bounded)
- **Impact:** High
- **Explanation:** Validation and the size calculation enforce only the minimum of 65,525 clusters. Nothing caps the count at 0x0FFFFFF5, so a small-cluster image grown onto a large device (4 KiB clusters past about 1 TiB, 512-byte clusters past about 128 GiB) "succeeds" with cluster numbers that collide with the BAD/EOC markers, and Linux refuses to mount it. No test covers it.
- **Evidence:** `src/resize/calculator.rs:184-190`, excerpt: "if calculation.new_data_clusters < BootSector::MIN_FAT32_CLUSTERS {"; `src/fat32/validation.rs:165-172`; `src/fat32/structs.rs:213` (no maximum constant)
- **Found by:** Security & Code Quality
- **Status:** Fixed
- **Status reason:** Added `BootSector::MAX_FAT32_CLUSTERS` (0x0FFFFFF5). Validation rejects a volume above it with `InvalidFAT32` (exit 5); `calculate_new_size` refuses growth past it with the new `Error::TooManyClusters` (exit 13, documented in README). Tests: `a_cluster_count_above_the_fat32_ceiling_is_rejected`, `growth_past_the_fat32_cluster_ceiling_is_refused`.
- **Status date:** 2026-10-10 07:44 UTC
- **Status commit:** 8e46f7f

### [F-T03] Phase 0 rewrites old cluster homes while the boot sector still looks valid
- **Category:** 26 Poor transactional boundaries
- **Impact:** Medium
- **Explanation:** The signature is invalidated only in phase 1, but phase 0 already overwrites the old homes of shifted clusters. After a crash, the filesystem mounts with the old geometry and scrambled contents; an automount, `fsck.fat` or a user write before the rerun makes the resume copy changed data. Data loss needs such an outside write, so the impact is Medium.
- **Evidence:** `src/resize/executor.rs:428`, excerpt: "// === PHASE 0: Data shift (safe - source preserved) ==="; first `invalidate_signature` at `src/resize/executor.rs:539`; TECHGUIDE.md:562-566
- **Found by:** Integration & Data
- **Status:** Fixed
- **Status reason:** `shift_cluster_data` now invalidates the boot sector and syncs right after the first checkpoint and its FSInfo mark are durable, before the first move; `DataShifted` carries the invalidated boot sector into the FAT phase, which no longer writes sector 0. `after_checkpoint_start` still sees a valid boot sector; `after_boot_invalidate` stays at the start of the FAT phase. Test: `a_crash_in_the_data_shift_leaves_a_volume_nothing_mounts`. TECHGUIDE/README wording is left to F-T06.
- **Status date:** 2026-10-14 07:13 UTC
- **Status commit:** 2566250

### [F-T04] The commit clears the checkpoint mark before the primary boot sector lands; FSInfo has two writers
- **Category:** 26 Poor transactional boundaries (also 27 Temporal coupling, 17 No clear ownership of data)
- **Impact:** Medium
- **Explanation:** `prepare_resize` clears the mark in the in-memory FSInfo about 470 lines before the commit, which writes and syncs it before the primary boot sector. After a crash at `BeforePrimaryBootWrite`, a device that changed size before the rerun is not found: `CheckpointSlot::locate` falls back to the new last sector and the run fails with `InvalidatedFilesystem`, which is the case the mark ([F-03]) exists for. Also, `mark_checkpoint_slot` read-modify-writes FSInfo from disk while the commit writes a stale in-memory copy.
- **Evidence:** `src/resize/executor.rs:173-176`, excerpt: "fsinfo.set_resize_record_sector(None);"; `src/resize/executor.rs:640-653`; `src/resize/checkpoint.rs:355-360` (`CheckpointSlot::locate`), `404-414` (`mark_checkpoint_slot`)
- **Found by:** Integration & Data, Coupling & Dependencies
- **Status:** Fixed
- **Status reason:** The early `set_resize_record_sector(None)` and the `fsinfo` field of `Prepared` are gone; the commit takes only the new free count and read-modify-writes FSInfo from disk like `mark_checkpoint_slot`, so the mark stays through the primary boot write. New `unmark_checkpoint_slot` removes it last, after `clear_checkpoint` and its sync. Tests: `a_crash_before_the_primary_boot_write_resumes_after_the_device_grew`, `a_crash_inside_a_commit_without_fat_growth_resumes_after_the_device_grew`. `the_commit_phase_writes_boot_backup_and_fsinfo_before_clearing_the_checkpoint` now finds the commit's FSInfo write before the clear (developer-approved) and also pins the one unmark write after it.
- **Status date:** 2026-10-14 09:05 UTC
- **Status commit:** 03a557a

### [F-T05] FAT mirroring flags and FS version are ignored
- **Category:** 30 Security as an afterthought (untrusted input not checked)
- **Impact:** Medium
- **Explanation:** `ext_flags` and `fs_version` have no caller. On a volume with mirroring disabled and FAT 2 active, the plan is made from the stale FAT 1, in-use clusters are not moved, and FAT 1 is then copied over the active FAT. The setup is rare, but the tool does not refuse it.
- **Evidence:** `src/fat32/structs.rs:135-145` (`ext_flags`, `fs_version`); `src/resize/executor.rs:353`, excerpt: "let fat = read_fat_table(&device, &boot, 0)?;"; `src/fat32/operations.rs:163-187` (`copy_fat_to_other_copies`)
- **Found by:** Security & Code Quality
- **Status:** Fixed
- **Status reason:** `resize` now calls `check_resizable_fat_layout` right after reading the boot sector, before any write: it refuses `fs_version != 0` and mirroring off (`ext_flags` bit 7) with an active FAT other than 0, as the new `Error::UnsupportedFatLayout` (exit 5, "not a usable FAT32 filesystem"; no new exit code). Mirroring off with FAT 0 active still resizes, since FAT 0 is then the live FAT. `info` is unchanged. Tests: `a_fat32_version_other_than_0_0_is_refused_and_writes_nothing`, `a_volume_whose_active_fat_is_not_fat_0_is_refused_and_writes_nothing`, `a_volume_with_mirroring_off_and_fat_0_active_grows_and_keeps_every_file`. README safety list and CHANGES updated.
- **Status date:** 2026-10-12 15:14 UTC
- **Status commit:** dbfc821

### [F-T06] Documentation contradicts the crash-safety model
- **Category:** 24 Inconsistent API contracts
- **Impact:** Medium
- **Explanation:** TECHGUIDE calls phase 0 "Safe: source data preserved" and later explains why that is false; its recovery pseudocode still reads the checkpoint from "last device sector" without the FSInfo mark. README calls phase 1 "the critical window", although phase 0 is the one that can end unrecoverable (F-T01, F-T03). This is residue that [F-29] missed.
- **Evidence:** TECHGUIDE.md:531, excerpt: "◄── Safe: source data preserved"; TECHGUIDE.md:586-603, 639-645; README.md:170; `src/resize/executor.rs:428`
- **Found by:** Integration & Data
- **Status:** Fixed
- **Status reason:** TECHGUIDE's phase diagram now draws the danger zone from the boot invalidation before the first move to the primary boot write, with the real commit order (FSInfo mark kept, removed after the checkpoint clear); "Boot Sector Invalidation" explains why the zone opens before the shift and uses `with_geometry`; the recovery pseudocode is replaced by the real rules (FSInfo mark, volume/geometry check, exit 9 cases); the recovery table has one row per crash point (8). README's phase list says data is moved, and the boot sector is invalid from phase 0 to the end of phase 2 instead of "Phase 1 (the critical window)". Docs only; the crash-point name guard test covers the table.
- **Status date:** 2026-10-14 09:35 UTC
- **Status commit:** 9ebfc04

### [F-T07] No test resizes a real block device
- **Category:** 32 Missing or inadequate test coverage for critical paths
- **Impact:** Medium
- **Explanation:** Every resize, including the QEMU suites, runs on an image file. On the main target (a grown partition), the `O_EXCL`/EBUSY → exit 4 path, `BLKGETSIZE64` sizing and the refusal of a mounted device are tested only with synthetic inputs, although the QEMU VM has root and loop devices.
- **Evidence:** `scripts/qemu/run-vm.sh:210`, excerpt: "/bin/fat32expander resize --verbose \"$FAT_IMAGE\""; `scripts/qemu/run-vm-multistage.sh:142, 183`; `scripts/qemu/test-crash-recovery.sh:281, 319`
- **Found by:** Security & Code Quality

### [F-T08] No crash point inside the shift loop
- **Category:** 32 Missing or inadequate test coverage for critical paths
- **Impact:** Low
- **Explanation:** The seven crash points sit between phases. A crash part-way through the longest phase, with a batch not yet synced, is tested only at unit level in the relocator, never end to end.
- **Evidence:** `src/resize/fault.rs:22-38` (`CrashPoint`); `src/resize/executor.rs:493-510`; `scripts/qemu/test-crash-recovery.sh:147`
- **Found by:** Security & Code Quality
- **Status:** Fixed
- **Status reason:** New crash point `before_mid_shift_record` fires part-way through the shift, after the moves since the first progress record are synced but before the second record covers them. `a_crash_in_the_middle_of_the_data_shift_resumes_from_started` and the QEMU crash suite (now 8 points) resume from it with files intact and the same geometry as an uninterrupted resize.
- **Status date:** 2026-10-10 07:14 UTC
- **Status commit:** 56d43ca

### [F-T09] Backup area: backup sector 0 breaks the commit order; backup FSInfo never written
- **Category:** 17 No clear ownership of data (also 26)
- **Impact:** Low
- **Explanation:** Validation accepts `backup_boot_sector == 0` as "no backup", but the commit still calls `write_backup_boot_sector(…, 0)`, which writes the primary before FSInfo and breaks the "primary written last" rule; the backup check then compares the primary with itself, in `prepare_resize` and in `info`. Separately, the backup FSInfo at `backup_boot_sector + 1` is never updated and keeps the pre-resize free count. Neither case has a test; a crash leaves stale hints, not data loss.
- **Evidence:** `src/fat32/validation.rs:69, 79-86`, excerpt: "Backup 0 means 'no backup'"; `src/fat32/operations.rs:104-106`; `src/resize/executor.rs:309-310, 637, 643`, excerpt: "write_fsinfo(device, fsinfo, record.fsinfo_sector)?;"; `src/resize/fsinfo.rs:19-21`
- **Found by:** Integration & Data, Security & Code Quality
- **Status:** Fixed
- **Status reason:** New `BootSector::backup_location()` returns `None` for field 0. With no backup, `prepare_resize` skips the check, `get_fs_info` reports `backup_matches` true without comparing, `info` prints "Backup boot sector: none", and the commit writes no backup, so the primary is again the last boot-sector write. The commit now sets the free count in the backup FSInfo at `backup + 1` when that sector is in the reserved area, is not the primary FSInfo and carries both FSInfo signatures; otherwise it is left alone. TECHGUIDE Step 7, section 7 and the phase diagram updated. Tests: safety net `the_sector_after_a_backup_in_the_last_reserved_sector_stays_a_fat` (9cd932e); new `a_volume_without_a_backup_writes_the_primary_only_at_the_commit_point`, `the_backup_fsinfo_carries_the_new_free_count`, `info_says_none_for_a_volume_without_a_backup_boot_sector`.
- **Status date:** 2026-10-15 14:47 UTC
- **Status commit:** 3be8551

### [F-T10] Public low-level API panics or over-allocates on crafted boot sectors
- **Category:** 30 Security as an afterthought
- **Impact:** Low
- **Explanation:** `BootSector::from_bytes` accepts `bytes_per_sector == 0`, and `first_data_sector`/`data_sectors` then divide by zero; the panic reaches `data_clusters`, `cluster_to_sector` and the `Debug` impl, whose own comment says it must not panic. `read_fat_table` pre-allocates from unvalidated fields. The CLI and resize paths are guarded; the public `fat32` API is not.
- **Evidence:** `src/fat32/structs.rs:276-278`, excerpt: "(self.root_entry_count() as u64 * 32).div_ceil(self.bytes_per_sector() as u64)"; `src/fat32/structs.rs:294-296, 357-360`; `src/fat32/operations.rs:209-210`
- **Found by:** Security & Code Quality

### [F-T11] Public API wider than any consumer; one function duplicated
- **Category:** 31 Dead code / unused dependencies (also 6 Leaky abstractions)
- **Impact:** Low
- **Explanation:** `count_free_clusters` has no production caller (its doc says re-export keeps it from the dead-code lint), `free_clusters_after_resize` reimplements it, and it panics on `fat.len() < 2`. `check_root` and `get_block_device_size` are root-exported "supported API" with no production caller, and the latter reopens by path, the TOCTOU pattern `block_device_size_of` rejects. The raw writers `write_boot_sector`, `write_backup_boot_sector` and `write_fsinfo` are public without the executor's checkpoint and sync discipline; eleven BPB accessors have no caller.
- **Evidence:** `src/fat32/operations.rs:237-245`, excerpt: "Being re-exported keeps it exempt from the dead-code lint"; `src/resize/executor.rs:686-705`; `src/system.rs:96-107, 162-170`; `src/lib.rs:67`; `src/fat32/mod.rs:34-38`
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Security & Code Quality

### [F-T12] Tool-private checkpoint mark lives in the public `FSInfo` type
- **Category:** 13 Inconsistent boundaries (also 6)
- **Impact:** Low
- **Explanation:** `FSInfo`, a format-layer type re-exported at the root, carries `RESIZE_RECORD_MARK` and public `resize_record_sector`/`set_resize_record_sector`. The checkpoint types were made private on purpose, yet an external caller can set or clear the recovery mark while it cannot read or write the record the mark points to.
- **Evidence:** `src/fat32/structs.rs:384-466`, excerpt: "pub fn set_resize_record_sector(&mut self, sector: Option<u64>)"; `src/lib.rs:62`; only reader `src/resize/checkpoint.rs:355` (`CheckpointSlot::locate`)
- **Found by:** Structure & Boundaries, Coupling & Dependencies

### [F-T13] "Nothing to do" is modelled as an error
- **Category:** 24 Inconsistent API contracts (also 9 Shotgun surgery)
- **Impact:** Low
- **Explanation:** The library returns `Err(Error::AlreadyMaxSize)` for a successful no-op, which CLAUDE.md calls "not an error". The binary special-cases it in three places, and every library caller must know that one `Error` variant is not an error.
- **Evidence:** `src/resize/calculator.rs:131-133, 164-166`; `src/main.rs:61`; `src/cli/exit.rs:49`; `src/cli/resize.rs:86`, excerpt: "!matches!(outcome, Err(Error::AlreadyMaxSize))"
- **Found by:** Structure & Boundaries, Error Handling & Observability

### [F-T14] CLI pre-check is a second open and an early gate
- **Category:** 25 Business logic in the UI
- **Impact:** Low
- **Explanation:** `cli/resize.rs` opens the device with `get_fs_info` before the library runs and decides continue-or-abort on its result. Because it runs before the library's mount check, a mounted non-FAT32 device exits 5, not 4; "Attempting recovery..." is printed under `--dry-run` too.
- **Evidence:** `src/cli/resize.rs:36-63`, excerpt: "Err(Error::BootSectorInvalidated) => { ... eprintln!(\"         Attempting recovery...\"); }"
- **Found by:** Error Handling & Observability

### [F-T15] Checkpoint trust policy split across two modules; rejection reason discarded
- **Category:** 11 Low cohesion (also 20 Weak error handling)
- **Impact:** Low
- **Explanation:** The invalidated-boot-sector case is decided in `checkpoint::check_for_incomplete_resize`, the valid-boot-sector case in a match in `prepare_resize`. There, the `CheckpointMismatch`/`CheckpointVolumeMismatch` detail is dropped, so the event says a record was ignored but never why.
- **Evidence:** `src/resize/executor.rs:255-281`, excerpt: "Err(_) => { report.notice(ResizeEvent::StaleCheckpointIgnored { sector: ..."; `src/resize/checkpoint.rs:438`
- **Found by:** Structure & Boundaries, Error Handling & Observability

### [F-T16] Library hard-wires the `/proc/mounts` check
- **Category:** 3 Tight coupling
- **Impact:** Low
- **Explanation:** `prepare_resize` always calls `check_not_mounted`, so a library caller where the mount table is unreadable cannot resize at all. The check is also exported for callers to run themselves, so the policy sits in two places.
- **Evidence:** `src/resize/executor.rs:26, 226`; `src/system.rs:12-18`; `src/lib.rs:67`
- **Found by:** Coupling & Dependencies

### [F-T17] Pure-logic modules depend on the checkpoint I/O module
- **Category:** 3 Tight coupling
- **Impact:** Low
- **Explanation:** The arithmetic-only calculator imports `ResizeCheckpoint`/`ResizePhase` to build and read records, so it depends on the module that imports `Device`, `read_fsinfo` and `write_fsinfo`. The relocator imports its pacing constant `PROGRESS_RECORD_EVERY` from `checkpoint`, the one edge that undercuts its otherwise closure-only seam.
- **Evidence:** `src/resize/calculator.rs:3, 62, 78`, excerpt: "use crate::resize::checkpoint::{ResizeCheckpoint, ResizePhase};"; `src/resize/relocator.rs:4, 216`; `src/resize/checkpoint.rs:75`
- **Found by:** Coupling & Dependencies

### [F-T18] Error messages repeat their cause or describe their trigger wrongly
- **Category:** 34 Inconsistent error/logging conventions
- **Impact:** Low
- **Explanation:** `MountTableUnreadable` and `Io` both interpolate `{source}` and mark it `#[source]`, so the cause prints twice, which the `DeviceOpen` doc forbids; `Io` also carries no device path. `CheckpointCorrupted` says "(CRC mismatch)" but also fires on an unknown phase byte, and `InvalidatedFilesystem` says "not found or corrupted" but means only "not found".
- **Evidence:** `src/error.rs:226`, excerpt: "failed to read /proc/mounts: {source}"; `src/error.rs:77-78, 150, 153`; `src/device.rs:179`; `src/resize/checkpoint.rs:315-317, 449-454`
- **Found by:** Error Handling & Observability

### [F-T19] One phase, several names in user-visible text
- **Category:** 34 Inconsistent error/logging conventions
- **Impact:** Low
- **Explanation:** A single run can print a phase as `name()` ("DataCopied"), as `{:?}`, as a number ("at phase 1") and as prose ("phase 1: data copied"). `ResizePhase::name()` duplicates the derived `Debug`.
- **Evidence:** `src/resize/checkpoint.rs:91-97` (`name`); `src/resize/executor.rs:296, 303`, excerpt: "ResizeSizeMismatch(checkpoint.phase as u8)"; `src/error.rs:157`
- **Found by:** Error Handling & Observability

### [F-T20] Progress and result API is mostly free text
- **Category:** 24 Inconsistent API contracts
- **Impact:** Low
- **Explanation:** Most events are `Step(String)` and the result carries `operations: Vec<String>`, so facts such as the new free count must be parsed from prose. `Resuming` carries the phase as `&'static str` because `ResizePhase` is private, while `Layout` and `ShiftStarting` are typed.
- **Evidence:** `src/resize/events.rs:19-26`, excerpt: "Step(String)", "Resuming { phase: &'static str"; `src/resize/options.rs:104`, excerpt: "pub operations: Vec<String>"
- **Found by:** Integration & Data

### [F-T21] Version-1 checkpoint read and write paths kept
- **Category:** 31 Dead code / unused dependencies
- **Impact:** Low
- **Explanation:** A second parse path and a version-1 write branch keep records from builds before [F-16] working, although the project has no users who could hold one. The write branch is unreachable: `ResizeCheckpoint::new` always sets `volume_id: Some`.
- **Evidence:** `src/resize/checkpoint.rs:36-39, 216-226, 306-331`; test `a_version_1_record_from_an_earlier_build_is_still_read` (`src/resize/checkpoint.rs:543`)
- **Found by:** Integration & Data, Security & Code Quality

### [F-T22] Leftover literal offsets
- **Category:** 28 Magic numbers/strings
- **Impact:** Low
- **Explanation:** [F-24] named the checkpoint offsets, but sub-field offsets in the progress and volume records, the FSInfo mark position, and the invalidated-signature value are still bare literals.
- **Evidence:** `src/resize/checkpoint.rs:265, 285-289`, excerpt: "get_u32(data, p + 8)"; `src/fat32/structs.rs:448-463`, excerpt: "self.raw[496..500]"; `src/fat32/validation.rs:26`, excerpt: "if sig == 0x0000"
- **Found by:** Error Handling & Observability

### [F-T23] Comment residue and report-ID density
- **Category:** 31 Dead code / unused dependencies (stale documentation)
- **Impact:** Low
- **Explanation:** `src/` carries about 200 bracketed report IDs in two schemes, many with long "used to" histories of removed designs that bury the current contract. At least one comment is plainly wrong: it calls `calculate_new_size` public, but it is `pub(crate)`.
- **Evidence:** `src/fat32/mod.rs:14-31`; `src/system.rs:103-106`; `src/cli/resize.rs:15-35`; `src/resize/calculator.rs:97-99`, excerpt: "but this function is public"
- **Found by:** Structure & Boundaries, Security & Code Quality

### [F-T24] CI and build tooling gaps
- **Category:** 32 Missing or inadequate test coverage (also 31)
- **Impact:** Low
- **Explanation:** `ci_workflow_test` guards the `device-journal` step but not the `fault-injection` crash-recovery step, which `--all-targets` would compile to zero tests. Clippy runs without `--all-targets`/`--all-features`. Two CI steps share the name "Run crash recovery tests", and a comment calls QEMU "the only test" of resume. The Makefile's `windows`, `dist` and `all-targets` targets cannot compile (unix-only imports), and an empty `mise.toml` is tracked.
- **Evidence:** `tests/ci_workflow_test.rs:91-111`; `.github/workflows/tests.yml:41, 121, 129, 181`; `Makefile:152`, excerpt: "cargo clippy -- -D warnings"; `src/device.rs:3`, excerpt: "use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};"; `mise.toml`
- **Found by:** Security & Code Quality

### [F-T25] Small misplaced responsibilities and an old dependency major
- **Category:** 10 Feature envy (also 11, 4)
- **Impact:** Low
- **Explanation:** The CLI renderer recomputes bytes per cluster although `BootSector::bytes_per_cluster` exists, and the tests of `BootSector::with_geometry` live in `executor.rs`, not in `structs.rs`. `thiserror` is pinned to 1.x while 2.x is the stable major.
- **Evidence:** `src/cli/render.rs:33-36, 77-79`, excerpt: "info.boot().bytes_per_sector() as u32 * info.boot().sectors_per_cluster() as u32"; `src/fat32/structs.rs:340`; `src/resize/executor.rs:707-756`; `Cargo.toml:27`, excerpt: "thiserror = \"1.0\""
- **Found by:** Structure & Boundaries, Coupling & Dependencies

## Coverage Checklist

### Flaw/Risk Types 1–34
| # | Type | Status | Finding |
|---|------|--------|---------|
| 1 | Global mutable state | Not observed | — (feature-gated test journal only; see S-T09) |
| 2 | God object | Not observed | — (largest files are mostly tests) |
| 3 | Tight coupling | Observed | F-T16, F-T17 |
| 4 | High/unstable dependencies | Observed | F-T25 |
| 5 | Circular dependencies | Not observed | — (one upward edge error→fat32, see S-T01) |
| 6 | Leaky abstractions | Observed | F-T11, F-T12 |
| 7 | Over-abstraction | Observed | F-T11 |
| 8 | Premature optimization | Not observed | — |
| 9 | Shotgun surgery | Observed (minor) | F-T13 |
| 10 | Feature envy / anemic domain model | Observed | F-T25 |
| 11 | Low cohesion | Observed | F-T15, F-T25 |
| 12 | Hidden side effects | Not observed | — |
| 13 | Inconsistent boundaries | Observed | F-T12 |
| 14 | Distributed monolith | Not applicable | — single process, one device |
| 15 | Chatty service calls | Not applicable | — no remote calls |
| 16 | Synchronous-only integration | Not applicable | — no integration partners |
| 17 | No clear ownership of data | Observed | F-T04, F-T09 |
| 18 | Shared database across services | Not applicable | — one writer, `O_EXCL` |
| 19 | Lack of idempotency | Observed | F-T01 |
| 20 | Weak error handling strategy | Observed (minor) | F-T15 |
| 21 | No observability plan | Not observed | — (see S-T10) |
| 22 | Configuration sprawl | Not observed | — (see S-T15) |
| 23 | Dependency injection misuse | Not observed | — |
| 24 | Inconsistent API contracts | Observed | F-T06, F-T13, F-T20 |
| 25 | Business logic in the UI | Observed | F-T14 |
| 26 | Poor transactional boundaries | Observed | F-T03, F-T04, F-T09 |
| 27 | Temporal coupling | Observed | F-T04 |
| 28 | Magic numbers/strings | Observed | F-T22 |
| 29 | "Utility" dumping ground | Not observed | — (`system.rs` mix covered in F-T11) |
| 30 | Security as an afterthought | Observed | F-T02, F-T05, F-T10 |
| 31 | Dead code / unused dependencies | Observed | F-T11, F-T21, F-T23, F-T24 |
| 32 | Missing test coverage for critical paths | Observed | F-T07, F-T08, F-T24 |
| 33 | Hard-coded credentials or secrets | Not observed | — |
| 34 | Inconsistent error/logging conventions | Observed | F-T18, F-T19 |

### Strength Categories S1–S14
| # | Category | Status | Finding |
|---|----------|--------|---------|
| S1 | Clear modular boundaries | Observed | S-T01 |
| S2 | High cohesion | Observed | S-T13 |
| S3 | Loose coupling | Observed | S-T09, S-T10 |
| S4 | Dependency direction is stable | Observed | S-T01 |
| S5 | Dependency management hygiene | Observed | S-T14 |
| S6 | Consistent API contracts | Observed | S-T07, S-T12 |
| S7 | Robust error handling | Observed | S-T07 |
| S8 | Observability present | Observed | S-T10 |
| S9 | Configuration discipline | Observed | S-T15 |
| S10 | Security built-in | Observed | S-T05, S-T06 |
| S11 | Testability & coverage | Observed | S-T08, S-T09 |
| S12 | Resilience patterns | Observed | S-T03, S-T04, S-T05, S-T11 |
| S13 | Domain modeling strength | Observed | S-T02 |
| S14 | Simple, pragmatic abstractions | Observed | S-T13 |

## Hotspots

1. `src/resize/executor.rs` — strong core (phase witnesses, barriers), but the remaining crash-safety gaps meet here: the sub-cluster shift (F-T01), the early phase-0 writes (F-T03), the early mark clear (F-T04) and the backup-0 commit order (F-T09).
2. `src/fat32/` (`validation.rs`, `structs.rs`, `operations.rs`) — the trust boundary for on-disk input: no maximum cluster count (F-T02), ignored `ext_flags` (F-T05), panics on crafted input (F-T10), and the widest part of the public API (F-T11, F-T12).
3. `TECHGUIDE.md`, `README.md` and the test harnesses (`scripts/qemu/`, `tests/crash_recovery_test.rs`) — the documented crash-safety model contradicts itself (F-T06), and the tests never reach the cases above or a real block device (F-T07, F-T08).

## Next Questions

1. Which geometries do real users hit (cluster size × growth), and how often does that give a shift shorter than one cluster?
2. Should the tool treat a FAT32 volume grown past the 0x0FFFFFF5-cluster limit as a refusal, or is that case out of scope?
3. Is a crash in phase 0 followed by a mount or `fsck.fat` before the rerun a realistic sequence on the target systems (automount, fstab)?
4. Which parts of the `fat32` namespace and the root API does any planned consumer need, and which exist only because tests name them?
5. Should the bracketed report IDs in the code be the long-term record of design history, or does that belong in the reports and commit messages?

## Analysis Metadata

- **Agents dispatched:** Structure & Boundaries; Coupling & Dependencies; Integration & Data; Error Handling & Observability; Security & Code Quality; one Verifier
- **Scope:** all of `src/` (23 files), `tests/` (12 files), `build.rs`, `Cargo.toml`, `Makefile`, `scripts/`, `.github/workflows/`, README.md, TECHGUIDE.md, CLAUDE.md
- **Raw findings:** 80 (26 strengths, 54 flaws)
- **Verified findings:** 40 (15 strengths, 25 flaws) after the verifier's merges and a final grouping of closely related items
- **Filtered out:** 40 — 4 dropped as false positives or below 60% confidence (shotgun surgery on new error variants, feature-gated test globals, library-side verbosity filter, a stale-doc sub-claim), the rest merged as duplicates
- **By impact:** strengths 8 high, 6 medium, 1 low; flaws 2 high, 5 medium, 18 low
- **Steering files consulted:** CLAUDE.md, TECHGUIDE.md, README.md, the two earlier reports in `paad/architecture-reviews/`, the project memory note on having no userbase
