# Architecture Report — fat32expander

**Date:** 2026-10-02
**Commit:** 6206b9275bd8581026c945559a036eec453ed106
**Languages:** Rust (2021 edition); clap, thiserror, anyhow, libc, crc32fast; shell (test scripts, QEMU harness)
**Key directories:** `src/`, `src/fat32/`, `src/resize/`, `tests/`, `scripts/`, `scripts/qemu/`, `.github/workflows/`
**Scope:** full repository

## Repo Overview

`fat32expander` is a Linux CLI and library that grows a FAT32 filesystem in place after its partition was enlarged. When the FAT must grow, the data area moves forward; the tool shifts every in-use cluster by whole sectors (cluster numbers are preserved), writes the larger FAT, then commits the new geometry. A checkpoint in the device's last sector, with CRC and a progress record, lets an interrupted run resume.

Size: 18 source files (about 5,900 lines including inline tests) plus 7 integration-test binaries (about 2,900 lines). Modules: `device` (sector I/O), `system` (mount check, ioctl), `fat32` (on-disk structures, validation, FAT read), `resize` (calculator, relocator, checkpoint, executor, options, fsinfo, fault injection), and `main.rs` (CLI). Dependency direction is acyclic: error ← system ← device ← fat32 ← resize ← main.

This is the second report. The previous one is `2026-07-28-fat32expander-architecture-report.md`; most of its High findings were fixed on this branch. Where a flaw below continues an open item of that report, the old ID is named.

## Strengths

### [S-01] Resize phase order is enforced by witness types
- **Category:** S13 Domain modeling strength (also S12)
- **Impact:** High
- **Explanation:** `DataShifted` → `FatExtended` → `FatOnDisk` are consumed by value, so the commit cannot run before the FAT phase and a resume can only build a witness from the previous one. The limit: the progress-record interval inside the shift is not type-enforced (see [F-01]).
- **Evidence:** `src/resize/executor.rs:44-121` (`FatOnDisk::recorded_by_an_earlier_run`), excerpt: "fn recorded_by_an_earlier_run(boot: BootSector, _shifted: DataShifted) -> Self"
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Integration & Data, Security & Code Quality

### [S-02] Durability barriers at the right points; CRC-protected checkpoint
- **Category:** S12 Resilience patterns
- **Impact:** High
- **Explanation:** The relocator syncs before it advances progress; the FAT is synced before the phase-2 checkpoint; supporting structures before the primary boot sector; the boot sector before the checkpoint is cleared. A CRC mismatch is an error, not "absent".
- **Evidence:** `src/resize/relocator.rs:237-241` (`execute_relocation`), excerpt: "if i > 0 { device.sync()?; } (progress.record)(mv.from_cluster)?"; `src/resize/executor.rs:551,636,649`; `src/resize/checkpoint.rs:216-218` "return Err(Error::CheckpointCorrupted)"
- **Found by:** Integration & Data, Security & Code Quality

### [S-03] Error taxonomy with an exhaustive exit-code mapping
- **Category:** S7 Robust error handling
- **Impact:** Medium (weakened by [F-05], [F-06])
- **Explanation:** One `thiserror` enum with structured variants (`DeviceIo` keeps path, offset, sector and source). `exit_code_for` is an exhaustive match, so a new variant forces an exit-code decision, and `tests/error_contract_test.rs` pins the codes. No `unwrap`/`expect` in non-test library code.
- **Evidence:** `src/main.rs:251-287` (`exit_code_for`), excerpt: "match e { Error::DeviceOpen { .. } => exit::DEVICE_UNOPENABLE,"; `src/error.rs`
- **Found by:** Error Handling & Observability, Integration & Data

### [S-04] Strong test suite wired into CI
- **Category:** S11 Testability & coverage
- **Impact:** Medium (weakened by [F-12], [F-13])
- **Explanation:** Relocator (17 unit tests, including independently computed sector math), checkpoint codec (18), integration (16), error contract (15), crash recovery from six crash points (9), durability ordering via the `device-journal` feature (8). CI runs `--all-targets`, `--ignored`, both feature suites and QEMU; `tests/ci_workflow_test.rs` fails if a test binary drops out of CI. `cargo clippy --all-targets` is clean.
- **Evidence:** `.github/workflows/tests.yml:57-121`; `tests/ci_workflow_test.rs`; `tests/durability_test.rs`
- **Found by:** Security & Code Quality

### [S-05] Two-level public API, private modules, acyclic dependencies
- **Category:** S1 Clear modular boundaries (also S4)
- **Impact:** Medium
- **Explanation:** Only `fat32` is a public module; everything else is reached through the crate root, and checkpoint types stay private. `main.rs` imports only root items.
- **Evidence:** `src/lib.rs:37-59`, excerpt: "mod device; mod error; pub mod fat32; mod resize; mod system;"; `src/main.rs:5` "use fat32expander::{get_fs_info, resize_fat32, Error, FSInfo, FSInfoReport, ResizeOptions};"
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Integration & Data

### [S-06] Callbacks keep the relocator unaware of the checkpoint format
- **Category:** S3 Loose coupling (also S14)
- **Impact:** Medium
- **Explanation:** `ShiftProgress` hands the relocator a recording closure, so `relocator.rs` imports nothing from `checkpoint`. `prepare_resize`/`Prepared` separates "read everything" from "mutate".
- **Evidence:** `src/resize/relocator.rs:175-184` (`ShiftProgress`), excerpt: "pub record: &'a mut dyn FnMut(u32) -> Result<()>"; `src/resize/executor.rs:123-133,478-490`
- **Found by:** Structure & Boundaries, Coupling & Dependencies

### [S-07] Probe closure keeps the device layer FAT-agnostic
- **Category:** S3 Loose coupling
- **Impact:** Medium
- **Explanation:** `Device::open_with` learns the sector size from a caller-supplied probe before `Self` exists, with a single open; the FAT knowledge stays in `fat32::operations`.
- **Evidence:** `src/device.rs:88-133`, excerpt: "pub fn open_with<P, F>(path: P, writable: bool, sector_size_from: F)"; `src/fat32/operations.rs:33-35` (`open_fat32_device`)
- **Found by:** Structure & Boundaries, Coupling & Dependencies

### [S-08] Checked layout accessors with one source of truth
- **Category:** S13 Domain modeling strength
- **Impact:** Medium
- **Explanation:** Layout questions on untrusted on-disk values return `Option` via checked arithmetic, and `with_geometry` is the single place that answers "what would the layout be".
- **Evidence:** `src/fat32/structs.rs:325,349`, excerpt: "pub fn data_clusters(&self) -> Option<u32>", "pub fn cluster_to_sector(&self, cluster: u32) -> Option<u64>"; `with_geometry` at `structs.rs:279`
- **Found by:** Structure & Boundaries, Security & Code Quality

### [S-09] Bounds-checked sector I/O, documented `unsafe`, least privilege
- **Category:** S10 Security built-in
- **Impact:** Medium (weakened by [F-10], [F-11], [F-17])
- **Explanation:** Sector reads and writes are checked against the device size with `checked_add`. The two `unsafe` blocks carry SAFETY arguments. The root gate was removed in favour of the kernel's own permission check.
- **Evidence:** `src/device.rs:211-219` (`check_bounds`); `src/system.rs:54-63,90-96`; `src/main.rs:330-341`
- **Found by:** Security & Code Quality

### [S-10] `ResizeOptions` builder with safe defaults and one narrow override
- **Category:** S9 Configuration discipline
- **Impact:** Low
- **Explanation:** All options default to the safe value; the only environment variable (`FAT32_CRASH_AT`) exists only under the `fault-injection` feature.
- **Evidence:** `src/resize/options.rs:68-76`; `src/resize/fault.rs:22-30`
- **Found by:** Error Handling & Observability

### [S-11] Lean dependencies; fault injection compiles to a no-op
- **Category:** S5 Dependency management hygiene
- **Impact:** Low (weakened by [F-27])
- **Explanation:** Five runtime dependencies, all used; `Cargo.lock` is tracked; no git or network dependencies.
- **Evidence:** `src/resize/fault.rs:33-37`, excerpt: "#[cfg(not(feature = \"fault-injection\"))] #[inline(always)] pub(crate) fn maybe_crash_at(_point: &str) {}"
- **Found by:** Coupling & Dependencies

### [S-12] Cohesive checkpoint and fsinfo modules
- **Category:** S2 High cohesion
- **Impact:** Low
- **Explanation:** `checkpoint.rs` holds the record, its codec and its sector I/O and nothing else; `fsinfo.rs` is a read-only query whose report holds a `BootSector` instead of copying fields.
- **Evidence:** `src/resize/checkpoint.rs:1-6`, excerpt: "everything about what a checkpoint *is* lives here"; `src/resize/fsinfo.rs:14-44`
- **Found by:** Structure & Boundaries

### [S-13] No global mutable state
- **Category:** S3 Loose coupling
- **Impact:** Low
- **Explanation:** The only static is a thread-local test journal behind the `device-journal` feature.
- **Evidence:** `src/device.rs:48-50`, excerpt: "thread_local! { static JOURNAL: RefCell<Vec<Op>> ..."
- **Found by:** Structure & Boundaries

## Flaws/Risks

### [F-01] Shift-progress interval is counted in moves, but the safety rule holds in clusters
- **Category:** 6 Leaky abstractions
- **Impact:** High
- **Explanation:** A replay after a crash is safe only while the recorded cluster is fewer than `shift_clusters` *clusters* above the lowest cluster already moved, because moving cluster `c` overwrites the old home of `c + shift_clusters`. The loop records every `record_every` *moves*, and only in-use clusters get a move, so on a filesystem with free gaps one interval spans more clusters than the rule allows; a resume then re-copies from a sector that already holds another cluster's data. Example with shift 2, in-use clusters 10 and 8: progress recorded at 10, 10 moved, 8 moved into 10's old home, crash; the resume re-reads 10's old home. The rule lives only in a doc comment on a public field, and the resume unit test uses dense clusters 2..9, so it cannot catch this. Derived from the code and traced by hand; not yet reproduced by a test.
- **Evidence:** `src/resize/relocator.rs:233-240` (`execute_relocation`), excerpt: "for (i, mv) in remaining.iter().enumerate() { if i % record_every == 0 {"; `relocator.rs:181-183` (`ShiftProgress::record_every`), excerpt: "How often to record progress, in moves. Must not exceed the shift distance in clusters"; `relocator.rs:~113` (`plan_relocation`), excerpt: "if fat_entry::is_free(entry) { continue; }"; `src/resize/executor.rs:443-458`, excerpt: "PROGRESS_RECORD_EVERY.min(shift_clusters)"; test data `relocator.rs:~598` "in_use = [2u32, 3, 4, 5, 6, 7, 8, 9]"
- **Found by:** Coupling & Dependencies (bound not enforced); unit mismatch found by the Verifier. Residue of the old [F-R1] fix.
- **Status:** Fixed
- **Status reason:** Reproduced by `execute_relocation_resumes_safely_across_free_clusters` (cluster 10 corrupted on resume). The interval is now a cluster distance, and `ShiftProgress::new` derives it from the plan (`min(PROGRESS_RECORD_EVERY, shift_clusters)`) with private fields, so code outside the relocator cannot pick an unsafe interval.
- **Status date:** 2026-10-03 UTC
- **Status commit:** 9c8ecfd

### [F-02] Resume adopts checkpoint geometry without comparing it to the boot sector
- **Category:** 26 Poor transactional boundaries
- **Impact:** Medium
- **Explanation:** On resume, `SizeCalculation` is copied from the checkpoint; the only guards are "device not smaller than `new_total_sectors`" and "geometry fits". Nothing compares `old_total_sectors`/`old_fat_size` with the boot sector just read, and the record carries no volume identity, so a stale or foreign record behind a valid boot sector drives `plan_relocation` and `init_new_fat_sectors` with mixed layouts (and can zero live FAT sectors). `calculator.rs:164` also multiplies `(new_fat_size - old_fat_size) * num_fats` unchecked on these values before the fit check.
- **Evidence:** `src/resize/executor.rs:288-301` (`prepare_resize`), excerpt: "SizeCalculation { old_total_sectors: checkpoint.old_total_sectors, ... old_fat_size: checkpoint.old_fat_size,"; `executor.rs:234-238` "if device_sectors < checkpoint.new_total_sectors as u64"; `executor.rs:707` "for sector_offset in calc.old_fat_size..calc.new_fat_size"; `src/resize/checkpoint.rs:279-302` (`check_for_incomplete_resize`)
- **Found by:** Integration & Data, Security & Code Quality
- **Status:** Fixed
- **Status reason:** `ResizeCheckpoint::check_against` refuses a record whose old geometry is not the boot sector's, or whose new geometry does not grow it, with `Error::CheckpointMismatch` (exit 9), before anything is written or a resume is announced. Reproduced first by `a_checkpoint_that_disagrees_with_the_boot_sector_is_refused` (exit 0 and a committed foreign geometry). `affected_cluster_range` saturates instead of overflowing. Not done: a volume identity in the record, which needs a format version bump and belongs with [F-16]. Revised 2026-10-07 on the user's decision: behind a valid boot sector a mismatching record is now ignored and the resize runs fresh (`a_stale_checkpoint_behind_a_valid_boot_sector_is_ignored`, which replaces the refusal test); behind an invalidated boot sector it is still refused (`a_mismatched_checkpoint_behind_an_invalidated_boot_sector_is_refused`).
- **Status date:** 2026-10-03 UTC
- **Status commit:** 85aa5ee

### [F-03] Checkpoint location follows the current device size; `ResizeSizeMismatch` cannot fire
- **Category:** 26 Poor transactional boundaries
- **Impact:** Medium
- **Explanation:** Write, read, clear and check each compute `device.total_sectors() - 1`, so if the device size changes between a crash and the next run the record is not found. After a phase-0 crash with a valid boot sector the next run replans the shift over partly shifted data; with an invalidated boot sector it fails as `InvalidatedFilesystem`. The size-mismatch guard compares against a record that could only be found at the new last sector, so it is unreachable in practice; no test constructs it.
- **Evidence:** `src/resize/checkpoint.rs:249,266,273,292`, excerpt: "let sector = device.total_sectors() - 1;"; `grep ResizeSizeMismatch tests/` → no hits
- **Found by:** Coupling & Dependencies, Integration & Data, Structure & Boundaries
- **Status:** Fixed
- **Status reason:** The checkpoint sector is chosen once per resize (`CheckpointSlot`, the device's last sector at the start) and marked in FSInfo's reserved bytes at 0x1F0 before any data moves; reruns follow the mark, and the commit's FSInfo write retires it. A device that shrank past the record is refused as `CheckpointOutsideDevice` (exit 9). Reproduced first by `a_crash_in_the_data_shift_resumes_after_the_device_grew` (fresh replan), `a_crash_in_the_danger_zone_resumes_after_the_device_grew` (exit 9) and `a_crash_in_the_data_shift_is_refused_after_the_device_shrank` (exit 0). After a real shrink, `CheckpointOutsideDevice` fires first. `ResizeSizeMismatch` is still reachable through a record that plans more sectors than the device has, and is a needed guard: without it such a record is committed with exit 0. It is now tested (`a_checkpoint_planning_beyond_the_device_is_refused`, 4a36506).
- **Status date:** 2026-10-05 UTC
- **Status commit:** 63c8312

### [F-04] Commit phase is not idempotent; the no-growth path has no recovery record
- **Category:** 19 Lack of idempotency
- **Impact:** Medium
- **Explanation:** `commit_new_geometry` re-reads the on-disk FSInfo free count and adds the new clusters, so any rerun after FSInfo landed (a `FatWritten` resume, or a no-growth rerun) counts them twice. On the no-growth path no checkpoint is written; a crash between the backup and primary boot sector writes leaves a `BackupMismatch` (exit 8), and the documented `--ignore-backup-mismatch` rerun then double-counts FSInfo.
- **Evidence:** `src/resize/executor.rs:609-631` (`commit_new_geometry`), excerpt: "let old_free = fsinfo.free_count(); ... old_free.saturating_add(additional_clusters)"; `executor.rs:348-349` "if !calculation.fat_needs_growth { return Ok((FatOnDisk::NotWritten(boot), 0)); }"
- **Found by:** Integration & Data. Continues [F-R26] (Partially fixed) and [F-R45].
- **Status:** Fixed
- **Status reason:** The FSInfo free count is derived from the FAT (`free_clusters_after_resize`, 020b53f), so a rerun of the commit gives the same value. A commit with no FAT phase behind it (no FAT growth, or an empty plan) now first writes a `FatWritten` checkpoint and marks it in FSInfo, so a crash between the backup and primary boot sector writes is resumed by a plain rerun; reproduced first by `a_crash_inside_a_commit_without_fat_growth_resumes_on_its_own` (exit 8). A resume no longer checks the backup against the primary, because the verified record names the primary's geometry and the commit rewrites the backup; this also applies to phase-0 resumes on the growth path. [F-20] itself is not fixed: an empty plan still commits the new FAT geometry.
- **Status date:** 2026-10-05 UTC
- **Status commit:** 03fc7eb

### [F-05] CLI re-implements library policy and so returns wrong exit codes
- **Category:** 25 Business logic in the UI
- **Impact:** Medium
- **Explanation:** `run()` opens the device a second time via `get_fs_info` and decides "can grow" and "backup mismatch" itself, duplicating `calculate_new_size` and `prepare_resize`. Because `can_grow()` is `device_sectors > total_sectors`, a device *smaller* than the filesystem prints "already at maximum size ... Nothing to do." and exits 0 instead of the documented 7. `exit::ALREADY_MAX_SIZE` (6) is unreachable, and "Nothing to do" exists twice.
- **Evidence:** `src/main.rs:344-375`, excerpt: "if !info.can_grow() { println!(\"Filesystem is already at maximum size"; `src/resize/fsinfo.rs:120-121` "self.device_sectors > self.boot.total_sectors() as u64"; `main.rs:265,297-299`
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Error Handling & Observability, Integration & Data. Continues [F-R41].
- **Status:** Fixed
- **Status reason:** `run()` no longer decides "can grow" or "backup mismatch"; the library's `AlreadyMaxSize`, `DeviceTooSmall` and `BackupMismatch` set the outcome, and `get_fs_info` is used only for display and to fail early, with the old message, on a filesystem that cannot be read. Reproduced first by `a_device_smaller_than_its_filesystem_exits_7` (exit 0, "Nothing to do"). Exit code 6 is retired like 11, the `--ignore-backup-mismatch` hint is part of the `BackupMismatch` message, and the size lines moved to the closing summary, from `ResizeResult`.
- **Status date:** 2026-10-04 UTC
- **Status commit:** a9199b4

### [F-06] Recovery refusals and environment failures map to the wrong exit codes
- **Category:** 20 Weak error handling strategy
- **Impact:** Medium
- **Explanation:** "Interrupted data shift left no progress record" and "Checkpoint geometry does not fit" are recovery states but are raised as `Error::Relocation`/`Error::Calculation` and exit 1 (internal) instead of 9 (needs recovery); the crash test for that path only checks `!success` and a substring. A `/proc/mounts` read failure is flattened into `Error::Io` and exits 10 ("device I/O") although no device was touched.
- **Evidence:** `src/resize/executor.rs:432-440`, excerpt: "Error::Relocation(\"interrupted data shift left no progress record"; `executor.rs:281-286` "Checkpoint geometry does not fit"; `src/main.rs:285` "Error::Calculation(_) | Error::Relocation(_) => exit::INTERNAL"; `tests/crash_recovery_test.rs:417-426`; `src/system.rs:12-17`
- **Found by:** Error Handling & Observability, Integration & Data
- **Status:** Fixed
- **Status reason:** New variants `ShiftProgressMissing` and `CheckpointGeometryUnusable` exit 9; `MountTableUnreadable` exits with the new code 12 (documented in the README) instead of 10. Reproduced first by `an_interrupted_sub_cluster_shift_exits_9` and `a_checkpoint_whose_new_geometry_does_not_fit_exits_9` (both exit 1). The mount check takes the mount table as an argument (`check_against_mounts`), so the read failure is unit-tested.
- **Status date:** 2026-10-05 UTC
- **Status commit:** 716cc37

### [F-07] The operations log is lost on every failure path
- **Category:** 21 No observability plan
- **Impact:** Medium
- **Explanation:** The audit trail is a local `Vec` returned only inside `Ok(ResizeResult)`; every `?` drops it, exactly when a record of the mutations already done matters.
- **Evidence:** `src/resize/executor.rs:137`, excerpt: "let mut operations = Vec::new();"; `executor.rs:184-191` "Ok(ResizeResult { ... operations, })"
- **Found by:** Error Handling & Observability. Continues [F-R19].
- **Status:** Fixed
- **Status reason:** Each step reaches the caller as a `ResizeEvent::Step` while the resize runs ([F-08]), so a library caller keeps them whatever the outcome. The CLI collects them and, on a failure other than "nothing to do", prints "Operations performed before the failure:" on stderr before the error.
- **Status date:** 2026-10-08 UTC
- **Status commit:** 9d2f3cc

### [F-08] No logging facade; the library writes to stderr directly
- **Category:** 21 No observability plan
- **Impact:** Medium
- **Explanation:** 14 `eprintln!` in `executor.rs` and 5 in `relocator.rs`, with `verbose: bool` threaded into `execute_relocation`; "Resuming interrupted resize ..." prints even without verbose. The CLI's verbose output goes to stdout, the library's to stderr, with no rule.
- **Evidence:** `src/resize/executor.rs:225-228`, excerpt: "eprintln!(\"Resuming interrupted resize from phase {:?}...\", checkpoint.phase);"; `src/resize/relocator.rs:203,216-248`; `src/main.rs:347-350` "println!(\"Current filesystem state:\")"
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Error Handling & Observability. Continues [F-R21], [F-R53].
- **Status:** Fixed
- **Status reason:** The library reports through `ResizeEvent`s handed to a caller's callback (`resize_fat32_with_events`; `resize_fat32` drops them) and prints nothing; `verbose` selects the detail events and the relocator takes the reporter instead of a `bool`. The CLI owns the output and its rule: events during the run to stderr, the finished report to stdout. Only the `fault-injection` crash line still prints from the library.
- **Status date:** 2026-10-08 UTC
- **Status commit:** e686c9b

### [F-09] `SizeCalculation` is a public data bag with dead and misleading members
- **Category:** 10 Feature envy / anemic domain model
- **Impact:** Medium
- **Explanation:** All fields are `pub` and exported through `ResizeResult::calculation`. `new_free_clusters`, documented as "free clusters after resize", holds a placeholder on a fresh run and 0 on a resume; `size_increase()` (unchecked subtraction), `new_size_bytes()`, `additional_clusters()` and `last_affected_cluster` have no production readers, and the export hides them from the dead-code lint. The checkpoint ↔ calculation mapping is hand-copied three times plus an inverse literal.
- **Evidence:** `src/resize/calculator.rs:128`, excerpt: "let new_free_clusters = additional_clusters; // Placeholder; actual = old_free + additional"; `src/resize/executor.rs:294` "new_free_clusters: 0, // Will be recalculated"; `calculator.rs:31-44`; `executor.rs:461-467,498-504,557-563` (`ResizeCheckpoint::new(...)`); `src/lib.rs:57`
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Error Handling & Observability, Integration & Data, Security & Code Quality
- **Status:** Fixed
- **Status reason:** `SizeCalculation` is crate-private and no longer exported; `ResizeResult` carries `old_total_sectors`, `new_total_sectors`, `old_fat_size` and `new_fat_size` instead. `new_free_clusters`, `last_affected_cluster`, `size_increase()`, `new_size_bytes()` and `additional_clusters()` are gone. The four hand-copied checkpoint constructions are `calculation.checkpoint(phase, volume_id)`, and the resume's inverse literal is `SizeCalculation::from_checkpoint`, built on the same `for_geometry` as a fresh calculation.
- **Status date:** 2026-10-08 UTC
- **Status commit:** c8372a9

### [F-10] Backup boot sector location is never validated; the documented override writes there blindly
- **Category:** 30 Security as an afterthought
- **Impact:** Medium
- **Explanation:** Validation range-checks neither `backup_boot_sector` nor `fs_info_sector`. A corrupt pointer into the FAT or data area produces `BackupMismatch`; the README's advice, `--ignore-backup-mismatch`, then makes the commit write a boot sector over that FAT or data sector. FSInfo is less exposed because its signatures are checked.
- **Evidence:** `src/fat32/validation.rs:17-160` (`validate_boot_sector_impl`) — no reference to `backup_boot_sector`; `src/fat32/operations.rs:77-80`, excerpt: "let data = device.read_sector(sector as u64)?; BootSector::from_bytes(&data)"; `operations.rs:96-97` (`write_backup_boot_sector`); `src/resize/executor.rs:242-255,609`
- **Found by:** Security & Code Quality
- **Status:** Fixed
- **Status reason:** `validate_boot_sector_impl` refuses an FSInfo sector outside `1..reserved_sectors`, and a backup boot sector that is neither 0 nor inside the reserved area, or that sits on FSInfo, with `BootSectorValidation` (exit 5) before any write. Reproduced first by `a_backup_pointer_into_the_fat_is_refused_even_with_the_override` (exit 0, boot sector written into the FAT) and `validation_rejects_fsinfo_and_backup_outside_the_reserved_area`.
- **Status date:** 2026-10-05 UTC
- **Status commit:** d0a0f56

### [F-11] Mount guard is racy, opens without `O_EXCL`, and misses loop-mounted images
- **Category:** 30 Security as an afterthought
- **Impact:** Medium
- **Explanation:** The `/proc/mounts` path comparison runs before a plain open, so a mount or a second concurrent run can start in between; a loop-mounted image appears as `/dev/loopN` and passes the check.
- **Evidence:** `src/system.rs:8-37` (`check_not_mounted`); `src/device.rs:96-99`, excerpt: "OpenOptions::new().read(true).write(writable).open(&path_buf)"
- **Found by:** Security & Code Quality. Continues [F-R27].
- **Status:** Fixed
- **Status reason:** A writable open of a block device now uses `O_EXCL`, so the kernel refuses it while the device is mounted or held and refuses a mount or a second resize while it is held; `EBUSY` there is the new `Error::DeviceBusy` (exit 4). A regular file is checked against the backing files of mounted loop devices (`/sys/block/loopN/loop/backing_file`). The `/proc/mounts` check stays for its message. The kernel's refusal itself is not tested: claiming a block device needs root.
- **Status date:** 2026-10-08 UTC
- **Status commit:** 27f443b

### [F-12] The no-FAT-growth path is untested although two tests claim it
- **Category:** 32 Missing test coverage for critical paths
- **Impact:** Medium
- **Explanation:** `test_resize_without_fat_growth` and `no_valid_checkpoint_survives_a_resize_without_fat_growth` use 128 → 150 MB, which takes the growth path, and neither asserts `!fat_grew`. No test anywhere asserts `fat_grew == false`, so the checkpoint-less commit ([F-04]) runs untested.
- **Evidence:** `tests/integration_test.rs:66-73`, excerpt: "Extend to 150MB (small increase, shouldn't require FAT growth)"; `tests/durability_test.rs:362-366`; only `fat_grew` asserts are `true` (`integration_test.rs:230,715`); `scripts/test-resize.sh:95` uses default `mkfs.fat -F 32`
- **Found by:** Security & Code Quality. Follow-up to [F-R56].
- **Status:** Fixed
- **Status reason:** `test_resize_without_fat_growth`, `no_valid_checkpoint_survives_a_resize_without_fat_growth` and the shell scenario "Simple Resize (No FAT Growth)" now grow a filesystem this tool already resized by 32 sectors, which keeps the FAT size, and each asserts that the FAT did not grow. A fresh `mkfs.fat` image cannot be used: its FAT is smaller than the resize formula's, so any increase grows it. The safety-net test `a_resize_without_fat_growth_adds_the_new_clusters_and_keeps_files` covers the free count and file contents on this path.
- **Status date:** 2026-10-03 UTC
- **Status commit:** 81d7ea3

### [F-13] No negative or fuzz tests for crash-recovery and untrusted-input paths
- **Category:** 32 Missing test coverage for critical paths
- **Impact:** Medium
- **Explanation:** No test feeds a checkpoint that disagrees with the boot sector ([F-02]), a moved device ([F-03]), a bad `backup_boot_sector` ([F-10]), or a sparse in-use pattern to the shift resume ([F-01]). No fuzz or property harness exists; FAT1/FAT2 equality is checked only via dosfsck.
- **Evidence:** `Cargo.toml` `[dev-dependencies]` — `tempfile = "3.0"` only; `grep ResizeSizeMismatch tests/` → none; `src/resize/relocator.rs:~598` "in_use = [2u32, 3, 4, 5, 6, 7, 8, 9]"
- **Found by:** Security & Code Quality. Continues [F-R12].
- **Status:** Fixed
- **Status reason:** The earlier tests (F-01, F-02, F-03, F-10 cases, see the previous status) are now joined by a `proptest` harness: the checkpoint codec (no panic on any bytes, round trip at every sector size, damage never yields a different record), boot sector and FSInfo parsing and the layout accessors (no panic; every data cluster of a valid geometry inside the volume), the size calculation on any and on valid geometries, and in `tests/property_test.rs` whole resizes of generated images with random in-use patterns (FAT1 = FAT2, every FAT entry and cluster kept, FSInfo free count). The whole-resize property found a FAT-sizing defect, fixed in eaf57d2. It also showed that damage to the progress record's magic reads as "no progress record" (a safe refusal, but reported as missing rather than damaged); not fixed here.
- **Status date:** 2026-10-08 UTC
- **Status commit:** 90970d9

### [F-14] A machine-specific `target` symlink is tracked in git
- **Category:** 22 Configuration sprawl
- **Impact:** Medium
- **Explanation:** `target` → `/scratch/oetiker/rust-targets/fat32expander` was committed in dc162d7 and exists only on this branch; on a fresh clone or CI runner it dangles, which breaks target-directory creation unless `CARGO_TARGET_DIR` is set. `.gitignore`'s `/target/` matches only directories.
- **Evidence:** `git ls-files target` → `target`; `ls -la target` → "target -> /scratch/oetiker/rust-targets/fat32expander"; `.gitignore:2` "/target/"; `git ls-tree main -- target` → empty
- **Found by:** Security & Code Quality
- **Status:** Fixed
- **Status reason:** The symlink is untracked (`git rm --cached`; the local link stays) and `.gitignore` says `/target`, which matches a symlink as well as a directory. A fresh clone of the commit builds with `CARGO_TARGET_DIR` unset.
- **Status date:** 2026-10-04 UTC
- **Status commit:** 3f9274f

### [F-15] Executor writes FAT areas as raw sectors and repeats FAT layout arithmetic
- **Category:** 13 Inconsistent boundaries
- **Impact:** Low
- **Explanation:** `init_new_fat_sectors` and `sync_fat_copies` compute FAT-copy positions in `resize`, while `read_fat_table` holds the same formula in `fat32`; no FAT write lives in `fat32::operations`. Both copies are correct today.
- **Evidence:** `src/resize/executor.rs:694-740`, excerpt: "let fat_dest_start = fat1_start + (fat_num as u64 * calc.new_fat_size as u64);"; `src/fat32/operations.rs:107` "boot.first_fat_sector() + (fat_number as u64 * boot.fat_size() as u64)"
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Integration & Data. Continues [F-R37].
- **Status:** Fixed
- **Status reason:** `fat_copy_start(boot, n, fat_size)` in `fat32::operations` is the one FAT layout formula; `read_fat_table` calls it with the boot sector's FAT size, the resize with the new one. `init_new_fat_sectors`/`sync_fat_copies` moved out of `executor.rs` into `operations.rs` as crate-private `zero_fat_extension` and `copy_fat_to_other_copies`, called from the same place. Safety net `tests/fat_io_test.rs` covers one- and two-FAT volumes (mutation-checked).
- **Status date:** 2026-10-08 UTC
- **Status commit:** pending

### [F-16] Checkpoint versioning fails open
- **Category:** 20 Weak error handling strategy
- **Impact:** Low
- **Explanation:** An unknown version returns `Ok(None)`, the same as "no checkpoint", so behind a valid boot sector a newer record is silently ignored and the shift replanned. The progress tail was added without a version bump, so an older build reads the record and replays the shift. Matters only on downgrade.
- **Evidence:** `src/resize/checkpoint.rs:207-211`, excerpt: "if version != CHECKPOINT_VERSION { return Ok(None); }"; `checkpoint.rs:22-26` "A build that predates the record ignores the tail and still reads the checkpoint"
- **Found by:** Error Handling & Observability, Integration & Data
- **Status:** Fixed
- **Status reason:** Our magic with an unknown version is refused with `CheckpointVersionUnsupported` (exit 9). The format is bumped to version 2, which appends the volume serial and its CRC at offset 44, behind the progress tail; `check_against` refuses a record of another volume with `CheckpointVolumeMismatch` (exit 9), closing the volume-identity gap [F-02] left. Revised 2026-10-07: behind a valid boot sector such a record is ignored, not refused (`a_checkpoint_from_another_volume_is_ignored`); see [F-02]. Version-1 records are still read (`a_version_1_record_from_an_earlier_build_is_still_read`). With approval, the "wrong version" case moved out of `checkpoint_reads_foreign_sectors_as_absent` into `a_checkpoint_of_an_unknown_version_is_refused`, and `checkpoint_rejects_corruption_in_any_covered_byte` and `checkpoint_content_is_independent_of_sector_size` follow the new behaviour and layout. Builds before this one still fail open on version 2; that cannot be fixed from here.
- **Status date:** 2026-10-07 UTC
- **Status commit:** bb661e3

### [F-17] Dead public API, including an unchecked raw write
- **Category:** 31 Dead code / unused dependencies
- **Impact:** Low
- **Explanation:** `Device::write_bytes_at` has no callers and would bypass the [F-R28] bounds check and the durability journal; `read_bytes_at` is unchecked too. `size_bytes`, `open_readonly_with_sector_size`, `get_block_device_size` are test-only; `BootSector::as_bytes_mut`, `FSInfo::as_bytes_mut`, `set_root_cluster`, `set_next_free` have no callers; `check_root` has none and its doc contradicts [F-R46]. Export hides all of it from the lint.
- **Evidence:** `src/device.rs:303-309`, excerpt: "pub fn write_bytes_at(&self, offset: u64, data: &[u8]) -> Result<()> { self.file.write_all_at(data, offset)"; `src/fat32/structs.rs:40,158,423,461`; `src/system.rs:52-53` "Check if running as root (required for block device access)"; `src/lib.rs:59`
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Security & Code Quality
- **Status:** Fixed
- **Status reason:** Deleted `Device::write_bytes_at` and `Device::size_bytes`, both `as_bytes_mut`, `BootSector::set_root_cluster`, `BootSector::fs_type` and `FSInfo::set_next_free`; `read_bytes_at` is `#[cfg(test)]`. `check_root`'s doc now says it is informational and that nothing requires root ([F-R46]). Developer's choice: `check_root`, `get_block_device_size` and `Device::open_readonly_with_sector_size` stay exported, because `tests/public_api_test.rs` pins them as supported API; removing them is an API decision, not dead-code cleanup. `RelocationPlan::is_empty` was already `#[cfg(test)]` ([F-20]).
- **Status date:** 2026-10-08 UTC
- **Status commit:** 228acba

### [F-18] FAT held twice in memory, with a stale chunk comment and a per-sector copy loop
- **Category:** 8 Premature optimization
- **Impact:** Low
- **Explanation:** `read_fat_table` reads into a byte buffer and then a second `Vec<u32>`; the "128KB" chunk comment is wrong for sectors larger than 512 bytes; `sync_fat_copies` copies one sector at a time. Allocation size is bounded by device size.
- **Evidence:** `src/fat32/operations.rs:111-126`, excerpt: "const CHUNK_SECTORS: u32 = 256; // Read 128KB at a time"; `src/resize/executor.rs:730-736`
- **Found by:** Security & Code Quality, Coupling & Dependencies. Continues [F-R6] (Partially fixed), [F-R58].

### [F-19] Filesystem-type check is an empty `if`
- **Category:** 31 Dead code
- **Impact:** Low
- **Explanation:** The condition is computed and its body is only a comment.
- **Evidence:** `src/fat32/validation.rs:152-157`, excerpt: "if !fs_type.starts_with(b\"FAT32\") && !fs_type.starts_with(b\"FAT\") { // This is a warning-level issue, not an error }"
- **Found by:** Security & Code Quality. Continues [F-R49].
- **Status:** Fixed
- **Status reason:** The empty `if` is gone; a comment says the label is not checked because the cluster count decides the FAT type. No behaviour change, pinned by `validation_ignores_the_fs_type_label`. `BootSector::fs_type` is left without callers and joins [F-17].
- **Status date:** 2026-10-06 UTC
- **Status commit:** f8554a6

### [F-20] An empty relocation plan still commits the new FAT geometry
- **Category:** 26 Poor transactional boundaries
- **Impact:** Low
- **Explanation:** With no in-use cluster in the affected range (root cluster marked free, i.e. a corrupt FAT, which validation does not reject), the FAT phase is skipped but the commit publishes `new_fat_size` over an unwritten FAT extension.
- **Evidence:** `src/resize/executor.rs:365-368`, excerpt: "if plan.is_empty() { ... return Ok((FatOnDisk::NotWritten(boot), 0)); }"
- **Found by:** Security & Code Quality, Coupling & Dependencies. Continues [F-R25].
- **Status:** Fixed
- **Status reason:** The early return on an empty plan is gone: a growing FAT always goes through phase 0 (nothing to move, checkpoints and FSInfo mark still written) and phase 1 (extension and FAT copies written), then the commit. `RelocationPlan::is_empty` is test-only now. The path was not unreachable as documented: the zero-FAT fixture `valid_image` took it in `a_cli_resize_of_a_growable_filesystem_succeeds_and_grows_it`. Red test `a_resize_with_nothing_to_shift_still_writes_both_fats`.
- **Status date:** 2026-10-07 UTC
- **Status commit:** c362455

### [F-21] Dry run skips crash detection
- **Category:** 24 Inconsistent API contracts
- **Impact:** Low
- **Explanation:** A dry run against an interrupted filesystem previews a fresh plan built from the invalidated boot sector, after the CLI printed "Attempting recovery...", so the preview is not what a real run would do.
- **Evidence:** `src/resize/executor.rs:218-222`, excerpt: "let incomplete_resize = if !options.is_dry_run() { check_for_incomplete_resize(&device, &boot)? } else { None };"
- **Found by:** Error Handling & Observability, Integration & Data. Continues [F-R32].
- **Status:** Fixed
- **Status reason:** `prepare_resize` runs `check_for_incomplete_resize` on a dry run too (it only reads; the dry run opens read-only), so the preview follows the resume path, its geometry and its refusals; the resume line reads "Dry run: an interrupted resize would be resumed from phase ...". Red test `a_dry_run_after_a_crash_previews_the_resume`; no-write pinned by `a_dry_run_after_an_early_crash_changes_nothing` and `a_dry_run_inside_the_danger_zone_changes_nothing`.
- **Status date:** 2026-10-07 UTC
- **Status commit:** 57bb43f

### [F-22] Sector-size knowledge duplicated, one copy wrong
- **Category:** 28 Magic numbers/strings
- **Impact:** Low
- **Explanation:** The user-visible error lists two of the four valid sizes; 4096 is defined twice; the sector-size check runs three times.
- **Evidence:** `src/error.rs:129`, excerpt: "Device sector size {0} is not supported (expected 512 or 4096)"; `src/fat32/operations.rs:10` "MAX_SECTOR_SIZE: usize = 4096"; `src/device.rs:71` "PROBE_BYTES: usize = 4096"; checks at `operations.rs:20-24,66-69`, `validation.rs:42-48`
- **Found by:** Structure & Boundaries, Coupling & Dependencies. [F-R52] is marked Fixed but is incomplete.
- **Status:** Fixed
- **Status reason:** The message takes its list from `BootSector::VALID_SECTOR_SIZES`. `MAX_SECTOR_SIZE` is gone: `read_boot_sector_unvalidated` reads sector 0 of a device already opened at the declared size and refuses a device addressed at another size, so the size list is checked at open and in the validator only. A const assertion ties `PROBE_BYTES` to the largest valid size. Red test `the_unsupported_sector_size_message_names_every_valid_size`; mismatch pinned by `a_boot_sector_is_read_only_at_the_size_it_declares`.
- **Status date:** 2026-10-06 UTC
- **Status commit:** 7543fcd

### [F-23] Crash-point names are bare strings across five files
- **Category:** 9 Shotgun surgery
- **Impact:** Low
- **Explanation:** Names repeat across `executor.rs`, `fault.rs`, TECHGUIDE, the QEMU script and the crash tests; a typo gives a crash point that silently never fires.
- **Evidence:** `src/resize/executor.rs:474,495,508,534,554,567`, excerpt: "maybe_crash_at(\"after_checkpoint_start\")"; `src/resize/fault.rs:23-29`
- **Found by:** Structure & Boundaries

### [F-24] Hand-written byte offsets and repeated unit literals
- **Category:** 28 Magic numbers/strings
- **Impact:** Low
- **Explanation:** The checkpoint codec uses raw offsets on both sides while the progress record beside it uses named constants; the CLI repeats `1024.0 * 1024.0` seven times and labels MiB as "MB".
- **Evidence:** `src/resize/checkpoint.rs:125-151,214-229`, excerpt: "data[12..16].copy_from_slice("; `src/main.rs` (`1024.0 * 1024.0`)
- **Found by:** Error Handling & Observability. Continues [F-R51].

### [F-25] `main.rs` mixes unrelated concerns
- **Category:** 11 Low cohesion
- **Impact:** Low
- **Explanation:** One file holds a hand-rolled date formatter, the report renderer, the exit-code table and a ~160-line Resize arm.
- **Evidence:** `src/main.rs:10-73` (`const_parse_u64`, `format_build_time`), `:133-222`, `:232-287`, `:324-481`
- **Found by:** Structure & Boundaries. Continues [F-R54].

### [F-26] Public types are not `#[non_exhaustive]` although stability is promised
- **Category:** 24 Inconsistent API contracts
- **Impact:** Low (crate is 0.0.1)
- **Explanation:** `lib.rs` says root items change only with a major version, yet `Error`, `ResizeResult` and `SizeCalculation` can be matched or constructed exhaustively, and `Error` has already gained and lost variants.
- **Evidence:** `src/lib.rs:12-13`, excerpt: "They change only with a major version."; `grep non_exhaustive src` → only `finish_non_exhaustive`
- **Found by:** Integration & Data
- **Status:** Fixed
- **Status reason:** `Error` and `ResizeResult` are `#[non_exhaustive]`; `ResizeEvent` already was ([F-08]) and `SizeCalculation` is no longer public ([F-09]). The other root types have private fields. `exit_code_for` in `main.rs` gained a wildcard arm; the compile-time "every variant has an exit code" guard it gave up moved into `every_variant_is_listed` in `src/error.rs`, an in-crate exhaustive match (verified by adding a probe variant: the lib tests stop compiling). `every_error_variant_keeps_its_exit_code` pins all 28 codes. The `lib.rs` API doc states the rule.
- **Status date:** 2026-10-08 UTC
- **Status commit:** 4785cf2

### [F-27] Library carries CLI-only dependencies
- **Category:** 31 Dead code / unused dependencies
- **Impact:** Low
- **Explanation:** `clap` (with derive) and `anyhow` are used only by `main.rs`, but library consumers compile them.
- **Evidence:** `Cargo.toml` `[dependencies]`, excerpt: "clap = { version = \"4.5\", features = [\"derive\"] }", "anyhow = \"1.0\""
- **Found by:** Coupling & Dependencies
- **Status:** Fixed
- **Status reason:** `clap` and `anyhow` are optional and enabled by a new default `cli` feature, which the `[[bin]]` requires. A library consumer opts out with `default-features = false`; `cargo build`, `cargo install`, the release workflow and the `CARGO_BIN_EXE` tests are unchanged. CI and the gate run `cargo check --lib --no-default-features`, the check that `lib.rs` does not use either crate; `tests/manifest_test.rs` pins the manifest shape and that CI step.
- **Status date:** 2026-10-08 UTC
- **Status commit:** 38ddc01

### [F-28] Build hash goes stale after a commit
- **Category:** 21 No observability plan (build provenance)
- **Impact:** Low
- **Explanation:** `.git/HEAD` holds a ref name that does not change on commit, so `version` can show an old `GIT_HASH` and build time.
- **Evidence:** `build.rs:30`, excerpt: "println!(\"cargo:rerun-if-changed=.git/HEAD\");"
- **Found by:** Coupling & Dependencies
- **Status:** Fixed
- **Status reason:** `build.rs` asks git for the paths of HEAD, the HEAD reflog, packed-refs and the current branch ref, and watches those that exist (a missing path would rebuild on every run). Works in worktrees. Red test `version_names_the_commit_it_was_built_from`, skipped without a git checkout. Needs git 2.31+ for `--path-format=absolute`; older git falls back to watching `build.rs` only.
- **Status date:** 2026-10-07 UTC
- **Status commit:** 773b646

### [F-29] Steering files and in-code docs have drifted from the code
- **Category:** 34 Inconsistent error/logging conventions (documentation)
- **Impact:** Low
- **Explanation:** CLAUDE.md's tree omits `checkpoint`, `fault`, `fsinfo`, `options`, `testutil`; it says the tool refuses maximum-size filesystems and mismatched backups (now exit 0 and `--ignore-backup-mismatch`) and that "All errors use thiserror" (CLI uses anyhow). TECHGUIDE shows non-`Option` accessor signatures, magic "0xFA32CHKP" (code: `b"FAT32RSZ"`), the old commit write order, and "Re-run entire resize" for `Started`. `checkpoint.rs:64` names the wrong checkpoint sector; `executor.rs:692-693` says nothing enforces the FAT order although the witnesses now do.
- **Evidence:** `CLAUDE.md` "Architecture" and "Error Handling" sections; `TECHGUIDE.md:366-367,488,578`; `src/resize/checkpoint.rs:64`, excerpt: "written to sector `old_total_sectors`"; `src/resize/executor.rs:692-693`, excerpt: "Nothing enforces the order but the call site in `resize_fat32`"
- **Found by:** all five specialists
- **Status:** Partially fixed
- **Status reason:** TECHGUIDE now gives the checkpoint magic as `FAT32RSZ`, and the `ResizeCheckpoint` doc in `checkpoint.rs` names its real location (the [F-03] slot). The rest is open: CLAUDE.md's tree and error-handling claims, TECHGUIDE's accessor signatures, commit write order and `Started` recovery text, and the `executor.rs` comment on FAT ordering.
- **Status date:** 2026-10-05 UTC
- **Status commit:** 4a36506

## Coverage Checklist

### Flaw/Risk Types 1–34
| # | Type | Status | Finding |
|---|------|--------|---------|
| 1 | Global mutable state | Not observed | — (see S-13) |
| 2 | God object | Not observed | — |
| 3 | Tight coupling | Not observed (secondary in F-05, F-15) | — |
| 4 | High/unstable dependencies | Not observed | — |
| 5 | Circular dependencies | Not observed | — |
| 6 | Leaky abstractions | Observed | #F-01 |
| 7 | Over-abstraction | Not observed | — |
| 8 | Premature optimization | Observed | #F-18 |
| 9 | Shotgun surgery | Observed | #F-23 (also F-09) |
| 10 | Feature envy / anemic domain model | Observed | #F-09 |
| 11 | Low cohesion | Observed | #F-25 |
| 12 | Hidden side effects | Observed (secondary) | #F-08, #F-21 |
| 13 | Inconsistent boundaries | Observed | #F-15 |
| 14 | Distributed monolith | Not applicable | single local process |
| 15 | Chatty service calls | Not applicable | no inter-service calls |
| 16 | Synchronous-only integration | Not applicable | no service integration |
| 17 | No clear ownership of data | Observed (secondary) | #F-05, #F-04 |
| 18 | Shared database across services | Not applicable | no database |
| 19 | Lack of idempotency | Observed | #F-04 |
| 20 | Weak error handling strategy | Observed | #F-06, #F-16 |
| 21 | No observability plan | Observed | #F-07, #F-08, #F-28 |
| 22 | Configuration sprawl | Observed | #F-14 |
| 23 | Dependency injection misuse | Not observed | — |
| 24 | Inconsistent API contracts | Observed | #F-21, #F-26 |
| 25 | Business logic in the UI | Observed | #F-05 |
| 26 | Poor transactional boundaries | Observed | #F-02, #F-03, #F-20 |
| 27 | Temporal coupling | Not observed as primary | — (phase order type-enforced, S-01) |
| 28 | Magic numbers/strings | Observed | #F-22, #F-24 |
| 29 | "Utility" dumping ground | Not observed | — |
| 30 | Security as an afterthought | Observed | #F-10, #F-11 |
| 31 | Dead code / unused dependencies | Observed | #F-17, #F-19, #F-27 |
| 32 | Missing test coverage for critical paths | Observed | #F-12, #F-13 |
| 33 | Hard-coded credentials or secrets | Not observed | — |
| 34 | Inconsistent error/logging conventions | Observed | #F-29 |

### Strength Categories S1–S14
| # | Category | Status | Finding |
|---|----------|--------|---------|
| S1 | Clear modular boundaries | Observed | #S-05 |
| S2 | High cohesion | Observed | #S-12 |
| S3 | Loose coupling | Observed | #S-06, #S-07, #S-13 |
| S4 | Dependency direction is stable | Observed | #S-05 |
| S5 | Dependency management hygiene | Observed | #S-11 |
| S6 | Consistent API contracts | Not observed | — (contradicted by F-05, F-06, F-09) |
| S7 | Robust error handling | Observed | #S-03 |
| S8 | Observability present | Not observed | — |
| S9 | Configuration discipline | Observed | #S-10 |
| S10 | Security built-in | Observed | #S-09 |
| S11 | Testability & coverage | Observed | #S-04 |
| S12 | Resilience patterns | Observed | #S-02 |
| S13 | Domain modeling strength | Observed | #S-01, #S-08 |
| S14 | Simple, pragmatic abstractions | Observed | #S-06 |

## Hotspots

1. `src/resize/executor.rs` and `src/resize/relocator.rs` — the crash-recovery core: home of the strongest design (S-01, S-02, S-06) and of every recovery-correctness risk (F-01 to F-04, F-20, F-21).
2. `src/resize/checkpoint.rs` — checkpoint identity, location and versioning (F-02, F-03, F-16) decide whether a resume acts on the right filesystem.
3. `src/main.rs` — duplicated policy and the exit-code contract (F-05, F-06, F-25).

## Next Questions

1. On a fragmented filesystem with a shift of only a few clusters, does a crash between progress records lead to a resume that copies wrong data ([F-01])?
2. Which filesystem should a checkpoint be allowed to resume, and what on disk proves that a found record belongs to the boot sector next to it ([F-02], [F-03])?
3. Which states should the exit-code contract classify as "needs recovery", and should the CLI or the library own the "nothing to do" decision ([F-05], [F-06])?
4. Is `fat32expander` meant to be used as a library by others, or only through its CLI? The answer sets the weight of [F-08], [F-09], [F-17], [F-26] and [F-27].
5. What should happen when a no-growth resize is interrupted between the backup and primary boot sector writes ([F-04], [F-12])?

## Analysis Metadata

- **Agents dispatched:** Structure & Boundaries; Coupling & Dependencies; Integration & Data; Error Handling & Observability; Security & Code Quality (ran `cargo clippy --all-targets` and `cargo test`, both clean); Verifier (one agent, all findings)
- **Scope:** all 18 files in `src/`, 7 files in `tests/`, `build.rs`, `Cargo.toml`, `.github/workflows/`, `scripts/`, steering docs
- **Raw findings:** 49 candidates after merging specialist reports (36 flaws, 13 strengths); about 60 specialist items before that merge
- **Verified findings:** 42 (29 flaws, 13 strengths); 3 candidates merged into others, plus one defect ([F-01]) newly surfaced by the verifier
- **Filtered out:** 4 (system.rs cohesion, phase-name formatting, checkpoint sync asymmetry, CI step names)
- **By impact:** flaws 1 high, 13 medium, 15 low; strengths 2 high, 7 medium, 4 low
- **Impact corrections by verifier:** F-02 High → Medium; F-01 Medium/Low → High; F-15 Medium → Low; F-17 Medium → Low; F-18 Low-Medium → Low
- **Steering files consulted:** `CLAUDE.md`, `TECHGUIDE.md`, `README.md`, `CHANGES.md`, previous report `2026-07-28-fat32expander-architecture-report.md`
