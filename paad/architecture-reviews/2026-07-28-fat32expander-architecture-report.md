# Architecture Report — fat32expander

**Date:** 2026-07-28
**Commit:** 8e40671ab36a813f0bf71f74cbb880f9ece042d6
**Languages:** Rust (edition 2021); Bash for the test harness
**Key directories:** `src/` (`fat32/`, `resize/`), `tests/`, `scripts/qemu/`, `.github/workflows/`
**Scope:** full repository

## Repo Overview

`fat32expander` is a Linux CLI tool that enlarges a FAT32 filesystem in place after its
underlying partition or disk image has been grown. It is a single Cargo package producing
both a library (`src/lib.rs`) and a binary (`src/main.rs`), ~3,700 lines of Rust across 15
source files plus ~14 shell scripts that drive a QEMU/Alpine integration harness.

The architecture is a clean four-layer stack: `error` ← `device`/`system` ← `fat32` ←
`resize`. `fat32/` parses and validates on-disk structures; `resize/` splits into a pure
`calculator`, a `relocator` that physically shifts cluster data, and an `executor` that
orchestrates the whole operation behind a checkpoint-based crash-recovery protocol.

The hard case the tool exists to solve is a resize large enough that the FAT tables must
grow. That pushes the start of the data area forward, so cluster data occupying the newly
needed FAT space must be shifted. The project went through a significant redesign: an
earlier approach renumbered clusters (updating FAT chains and directory entries); the
current one preserves cluster numbers and moves only physical sectors. Much of what this
report flags is residue from that transition — dead APIs, stale documentation, and, most
importantly, two places where the old and new models of "where does the data area start"
still disagree.

Five specialist agents analysed the repository in parallel; a verifier then re-read the
code at every cited location. Of 60 candidate flaws, 57 were verified, 1 dropped as
double-counting, 1 resolved into a strength, and 1 merged. 74 of 75 findings are
CONFIRMED by direct code reading; 1 is PLAUSIBLE. Two of the High-impact findings were
additionally reproduced empirically against real FAT32 images, and two more were
reproduced against crafted ones.

> **Note on this report's most serious findings.** R2 and R1 describe silent data
> corruption reproduced on real filesystems. R2 in particular fires on an ordinary,
> crash-free resize with no unusual input — only an unlucky geometry. Both were verified
> twice: once empirically by the specialist, once by the verifier re-deriving the
> arithmetic from the code independently.

## Strengths

### [S-A] Strictly acyclic layering: error ← device/system ← fat32 ← resize
- **Category:** S1 (clear modular boundaries) / S4 (stable dependency direction)
- **Impact:** High
- **Explanation:** Every module's imports point strictly downward, with no back-edges or cycles anywhere in the crate — unusual discipline for a project of this size.
- **Evidence:** `src/error.rs:1` imports only `thiserror`; `src/device.rs:1` (`use crate::error::{Error, Result};`) is that file's sole crate import; `src/fat32/operations.rs:1-6` imports device + error + fat32; `src/resize/executor.rs:1-10` adds system. No file in `device.rs`, `system.rs` or `fat32/structs.rs` references `resize`.
- **Found by:** Structure & Boundaries, Coupling & Dependencies

### [S-B] Dry-run enforced twice — by control flow and by the OS
- **Category:** S3 (loose coupling) / S7 (robust error handling)
- **Impact:** High
- **Explanation:** Dry-run opens the device without write permission *in addition* to guarding every write behind a conditional, so a missed guard fails at the kernel rather than corrupting a filesystem. Genuine defence in depth.
- **Evidence:** `src/resize/executor.rs:329-333` (`resize_fat32`), excerpt: `let mut device = if options.is_dry_run() { Device::open_readonly(...)? } else { Device::open(...)? };`; writes additionally gated at `:476` and `:574`; `get_fs_info` always read-only at `:710`. The Error Handling specialist traced every path reachable under dry-run and found no write call outside a guard.
- **Found by:** Error Handling & Observability, Coupling & Dependencies, Security & Code Quality, Integration & Data (4 specialists)

### [S-C] Versioned, CRC-protected checkpoint plus boot-signature invalidation as a cross-tool interlock
- **Category:** S7 (robust error handling) / S10 (security built-in) / S12 (resilience) / S14 (pragmatic abstractions)
- **Impact:** High
- **Explanation:** Crash recovery rests on a 32-byte record — magic, version, phase, four `u32`s, CRC32 — written to the last sector of the device, deliberately chosen so shifted cluster data cannot clobber it; the boot signature is zeroed across the unsafe window so `fsck.fat` and the kernel driver refuse a half-resized volume.
- **Evidence:** `src/resize/executor.rs:117-147` (`ResizeCheckpoint::to_bytes`), excerpt: `let crc = crc32fast::hash(&data[0..Self::DATA_SIZE]); data[28..32].copy_from_slice(&crc.to_le_bytes());`; `:535-537` `boot.invalidate_signature(); write_boot_sector(&device, &boot)?; device.sync()?;`; sector-placement rationale documented at `:198-203`. Version/CRC failures correctly distinguish "no checkpoint" (`Ok(None)`, `:161-168`) from "corrupt checkpoint" (`Err(Error::CheckpointCorrupted)`, `:174`).
- **Found by:** Error Handling & Observability, Security & Code Quality, Integration & Data, Structure & Boundaries (4 specialists)
- **Qualified by:** [F-R17], [F-R18] — the sync discipline is not uniform; only four `sync()` sites exist crate-wide.

### [S-D] CI actually runs the `#[ignore]`d integration tests, the shell suite, and a real QEMU VM suite
- **Category:** S11 (testability & coverage)
- **Impact:** High
- **Explanation:** Contrary to the common pattern where `#[ignore]`d tests silently never run, the workflow passes `-- --ignored` explicitly, then boots an Alpine VM to exercise the kernel's own `vfat` driver against resized images.
- **Evidence:** `.github/workflows/tests.yml:57-123`, excerpt: `cargo test --test integration_test -- --ignored 2>&1 | tee integration-test-results.txt` / `INTEGRATION_EXIT=${PIPESTATUS[0]}` — true exit codes propagate rather than being masked by the `tee` pipeline. Nine QEMU scenarios in `scripts/qemu/tests/` run on every push and PR to main.
- **Found by:** Security & Code Quality
- **Qualified by:** [F-R9] — the crash-recovery suite specifically is excluded by the discovery glob and never runs.

### [S-E] Compile-gated fault injection with named crash points and a crash/recover/verify VM harness
- **Category:** S3 / S9 (config discipline) / S11 / S12 (resilience)
- **Impact:** High
- **Explanation:** Six named crash points span the danger zone, injectable only under a non-default cargo feature that compiles to an `#[inline(always)]` no-op otherwise — a real `exit(137)` mid-write rather than a mock, at zero cost in shipped binaries.
- **Evidence:** `src/resize/executor.rs:40-44`, excerpt: `#[cfg(not(feature = "fault-injection"))] #[inline(always)] fn maybe_crash_at(_point: &str) { }`; `Cargo.toml:11` `default = []`; harness at `scripts/qemu/test-crash-recovery.sh:283` diffs SHA-256 sums of every file after remount. This is the mechanism that let the Integration specialist confirm and bound [F-R1].
- **Found by:** Error Handling & Observability, Coupling & Dependencies, Security & Code Quality, Integration & Data (4 specialists)

### [S-F] BootSector/FSInfo model on-disk sectors as read-modify-write byte containers
- **Category:** S13 (domain modeling strength)
- **Impact:** High
- **Explanation:** Both types retain the full raw sector and expose typed accessors over it, rather than deserializing into a Rust struct and re-serializing — which would silently destroy boot code, OEM name and vendor-specific bytes. Exactly right for a tool that rewrites live filesystem metadata.
- **Evidence:** `src/fat32/structs.rs:129-132` (`BootSector`), excerpt: `pub struct BootSector { /// Full sector data (512 to 4096 bytes depending on sector size) raw: Vec<u8>, }`; comment at `:146` `// Store the full sector data for read-modify-write`; write path `src/fat32/operations.rs:83` `device.write_sector(0, boot.as_bytes())`.
- **Found by:** Structure & Boundaries

### [S-Q] `Device` gives a hermetic file-backed test seam, but no trait-level seam
- **Category:** S11 (testability & coverage)
- **Impact:** Medium
- **Explanation:** `open_impl` treats a regular file and a block device uniformly, so the entire resize pipeline runs against a temp-file image with no root and no loop devices — the seam exists and is exercised; the untested code in [F-R10]/[F-R11] is a choice, not a constraint.
- **Evidence:** `src/device.rs:39-46` (`Device::open_impl`), excerpt: `let size = if metadata.is_file() { metadata.len() } else { let mut f = file.try_clone()?; f.seek(SeekFrom::End(0))? };`; `tests/integration_test.rs:6-28` builds images with `truncate` + `mkfs.fat` and verifies with `dosfsck -n` — an independent implementation, not the tool's own notion of correctness.
- **Found by:** Security & Code Quality (as a strength), Coupling & Dependencies (as a flaw) — **reconciled during verification**
- **Note:** Two specialists reached opposite conclusions here. The verifier resolved it: the premise that `Device` is a concrete struct over `std::fs::File` with no trait is correct, but the consequence ("relocation and checkpoint logic cannot be exercised against an in-memory image") is refuted — they can be, they simply aren't. The genuine residual limitation is narrower: without a trait there is no way to inject I/O faults or assert write ordering, which is exactly why [F-R17]/[F-R18] are unfalsifiable by test.

### [S-G] Size calculation is a pure function of `&BootSector` — no I/O dependency
- **Category:** S3 (loose coupling)
- **Impact:** Medium
- **Explanation:** The sizing layer takes a parsed boot sector and a sector count and returns a plain data struct, with no `Device` in any signature — which is why it is the one module with meaningful unit tests.
- **Evidence:** `src/resize/calculator.rs:1-2` imports only `crate::error` and `crate::fat32::BootSector`; `pub fn calculate_new_size(boot: &BootSector, device_sectors: u64) -> Result<SizeCalculation>`; tests at `:236-279` exercise growth, shrink-rejection and same-size paths with no filesystem.
- **Found by:** Coupling & Dependencies

### [S-H] Validation is a distinct pure layer invoked on the read path, not left to callers
- **Category:** S2 (high cohesion) / S10 (security built-in)
- **Impact:** Medium
- **Explanation:** A caller cannot obtain an unvalidated `BootSector` or `FSInfo` through the normal API, and the sector-size bootstrap is handled defensively rather than trusting the declared value.
- **Evidence:** `src/fat32/operations.rs:21-37` (`read_boot_sector`), excerpt: `if !VALID_SECTOR_SIZES.contains(&sector_size) { return Err(Error::UnsupportedSectorSize(sector_size as u32)); }` — 4096 bytes read first, size checked against an allowlist, only then trimmed and re-parsed, then `validate_boot_sector(&boot)?`. `validation.rs` is pure (no I/O, no device, no state), and the recovery variant is one parameterized flag over a shared `_impl` (`:5-17`) rather than a forked copy.
- **Found by:** Security & Code Quality, Structure & Boundaries
- **Qualified by:** [F-R8] — the gap is *what* is validated, not *whether* validation is wired in.

### [S-I] Structured operations log returned as data, not printed from the library
- **Category:** S8 (observability present)
- **Impact:** Medium
- **Explanation:** `ResizeResult.operations` is a machine-readable ordered list of every step performed, produced by the library and rendered by the CLI, so an embedder gets the same record the CLI prints — the right shape for an audit trail.
- **Evidence:** `src/resize/executor.rs:303-318`, excerpt: `/// List of operations performed (for logging) pub operations: Vec<String>`; rendered at `src/main.rs:246-249` `for op in &result.operations { println!("  - {}", op); }`.
- **Found by:** Error Handling & Observability
- **Qualified by:** [F-R19] (lost on every error path) and [F-R20] (one entry asserts a check that never ran) — heavily.

### [S-J] Minimal mainstream dependency set, lockfile committed, lint gate in CI, non-network build script
- **Category:** S5 (dependency hygiene) / S10 (security built-in)
- **Impact:** Medium
- **Explanation:** Five direct runtime dependencies resolving to 39 packages, every one actually used, with `Cargo.lock` tracked and a clippy gate enforced in both CI and the Makefile.
- **Evidence:** `Cargo.toml:16-24` — clap + anyhow in `main.rs`, thiserror in `error.rs`, libc in `system.rs`, crc32fast in the checkpoint CRC; no unused entries. `.github/workflows/tests.yml:41` `cargo clippy -- -D warnings`; `build.rs` is 31 lines that stamp a timestamp and `git rev-parse --short HEAD`, download nothing, and degrade to `"unknown"` without git.
- **Found by:** Coupling & Dependencies, Security & Code Quality
- **Caveats:** `thiserror = "1.0"` is a major version behind; the release workflow pins only `dtolnay/rust-toolchain@stable`, so published binaries build from whatever `stable` resolves to that day; and per [F-R44] the `-D warnings` gate structurally cannot catch the unused public API.

### [S-K] Binary consumes only the crate facade, never a submodule path
- **Category:** S3 (loose coupling) / S6 (consistent API contracts)
- **Impact:** Medium
- **Explanation:** Despite every module being `pub`, the CLI imports exactly four symbols from the crate root, so the lib/bin split is real rather than nominal.
- **Evidence:** `src/main.rs:5` `use fat32expander::{check_root, get_fs_info, resize_fat32, ResizeOptions};` is the only crate-internal import; `tests/integration_test.rs:1` follows the same discipline. Every fallible operation returns `Result<T, Error>` over one `thiserror` enum (`src/error.rs:71`).
- **Found by:** Coupling & Dependencies, Integration & Data

### [S-L] Positional I/O with sector-size validation as the single device contract
- **Category:** S6 (consistent API contracts)
- **Impact:** Medium
- **Explanation:** All I/O goes through `read_exact_at`/`write_all_at` on `&self` — no shared seek cursor to corrupt — and `write_sector` rejects any buffer whose length differs from the sector size.
- **Evidence:** `src/device.rs:130-139` (`write_sector`), excerpt: `if data.len() != self.sector_size as usize { return Err(Error::Io(...)) }`.
- **Found by:** Integration & Data
- **Note:** [F-R28] is the complement — buffer *length* is checked, sector *position* is not.

### [S-M] `ResizePhase` as an ordered enum encoding the recovery state machine
- **Category:** S13 (domain modeling strength)
- **Impact:** Medium
- **Explanation:** The three-phase recovery protocol is modeled as an ordered `#[repr(u8)]` enum, so resume logic reads as a direct statement about progress rather than an integer comparison.
- **Evidence:** `src/resize/executor.rs:55-57`, excerpt: `#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)] #[repr(u8)] pub enum ResizePhase {`, with `from_u8` rejecting out-of-range at `:67-74`; used at `:532` `if starting_phase <= ResizePhase::DataCopied {`.
- **Found by:** Structure & Boundaries
- **Qualified by:** [F-R15] — the enum orders the phases but does not enforce the transitions.

### [S-N] Validation errors name the offending field and the expected value
- **Category:** S7 (robust error handling)
- **Impact:** Medium
- **Explanation:** Validation failures include observed and expected values, and structured variants carry data rather than prose.
- **Evidence:** `src/fat32/validation.rs:44` `"Invalid sectors per cluster: {} (must be power of 2, 1-128)"`; `:119` `"Cluster count {} indicates FAT12/16, not FAT32 (need >= 65525)"`; `src/error.rs` `#[error("Device '{0}' is currently mounted at '{1}'")] DeviceMounted(String, String)`; `main.rs` adds the device path via `.with_context(...)`.
- **Found by:** Error Handling & Observability
- **Note:** Uneven — [F-R29] and [F-R30] are the counter-cases on the I/O paths.

### [S-O] Protocol constants named and centralized; byte offsets encapsulated behind accessors
- **Category:** S9 (configuration discipline) / S14 (pragmatic abstractions)
- **Impact:** Medium
- **Explanation:** FAT entry semantics, FSInfo signatures, sector-size limits and checkpoint constants are all named, and boot-sector byte offsets appear as literals only inside single-purpose accessors — no call site outside `structs.rs` indexes raw bytes.
- **Evidence:** `src/fat32/structs.rs:445-453` `pub const END_OF_CHAIN_MIN: u32 = 0x0FFFFFF8;`; `:539-582` `mod fat_entry` gives readable call sites (`fat_entry::is_free(entry)`) at zero abstraction cost; accessor doc comments state the offset, e.g. `/// Root directory cluster (offset 44, 4 bytes)`.
- **Found by:** Error Handling & Observability, Structure & Boundaries
- **Counter-cases:** [F-R51] (checkpoint codec), [F-R52] (`validation.rs` literals).

### [S-P] Layered pre-flight refusals; three of four documented refusals verified unbypassable
- **Category:** S7 (robust error handling) / S12 (resilience patterns)
- **Impact:** Medium
- **Explanation:** CLAUDE.md's documented refusals were checked against code: mounted, non-FAT32, already-at-max and shrink are all real and not bypassable by any flag.
- **Evidence:** `src/resize/executor.rs:325` `check_not_mounted(options.device_path())?;` is the first statement of `resize_fat32`, before the device is even opened; `src/resize/calculator.rs:70-72` `if new_total_sectors == old_total_sectors { return Err(Error::AlreadyMaxSize); }`; `:65-67` `Error::ShrinkNotSupported`.
- **Found by:** Integration & Data, Error Handling & Observability
- **Caveats:** the fourth documented refusal (backup boot sector) is stronger at library level than the CLI advertises — [F-R4]. Loop-mounted images are not detected — [F-R27].

### [S-R] No hard-coded secrets present
- **Category:** S10 (security built-in)
- **Impact:** Low
- **Explanation:** A repo-wide credential sweep across source, workflows, Makefile, docs and the QEMU scripts found nothing but GitHub's own injected token.
- **Evidence:** `grep -rniE 'password|secret|token|api[_-]?key|ssh-rsa|BEGIN .*PRIVATE KEY|credential'` over `scripts/ .github/ src/ Makefile README.md TECHGUIDE.md` → only `.github/workflows/release.yml:338,517` `GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}`. The QEMU harness boots an Alpine initramfs over serial with no login, so there are no VM passwords or SSH keys to leak.
- **Found by:** Security & Code Quality

## Flaws/Risks

### [F-R2] Clean resize writes cluster data to the wrong sectors when `sectors_per_cluster` does not divide the total FAT growth
- **Category:** 17 (no clear ownership of data)
- **Impact:** High
- **Explanation:** `plan_relocation` derives the new data-area start by rounding the FAT growth **up to a whole cluster**, while the boot sector committed at the end of `resize_fat32` defines it as `reserved + num_fats * new_fat_size` — the un-rounded value. When `spc ∤ (num_fats × Δfat_size)`, every relocated cluster lands past where the new boot sector says it is.
- **Evidence:** `src/resize/relocator.rs:68-70` (`plan_relocation`), excerpt: `let shift_sectors = affected_clusters * sectors_per_cluster; let new_first_data_sector = old_first_data_sector + shift_sectors;` — where `affected_clusters = total_growth.div_ceil(sectors_per_cluster)` (`calculator.rs:113`). Against `src/fat32/structs.rs:381-388` `first_data_sector()` = `reserved_sectors + (num_fats * fat_size) + root_dir_sectors`, moved only by `executor.rs:578-579` `boot.set_fat_size_32(calculation.new_fat_size);`.
- **Reproduced:** 256 MB image, `mkfs.fat -F 32 -s 4`, fat_size 1020 → 1023. A single clean `resize --force` — no fault injection — yields `fsck.fat`: `/FSCK0000.017 File size is 450489715 bytes, cluster chain length is 0`, a garbage volume label, 52 files instead of 20, and mismatched checksums. The verifier re-derived the arithmetic independently: reserved=32, num_fats=2, spc=4 ⇒ growth = 6 sectors, `ceil(6/4) = 2` clusters ⇒ shift = 8. Boot-implied `first_data_sector` = **2078**; relocator's = **2080**. Data lands **2 sectors past** where the committed boot sector points.
- **Found by:** Integration & Data
- **Verdict:** CONFIRMED — the two formulas are genuinely different expressions, and the divergence was reproduced both empirically and numerically. They agree only when `spc | growth`, which is why every CI geometry passes.
- **Status:** Fixed
- **Status reason:** `plan_relocation` now takes the boot sector as it will be committed (`new_boot`) and reads `new_boot.first_data_sector()` directly instead of deriving a shift from a rounded-up cluster count, so the plan cannot disagree with the geometry the filesystem ends up with. The ignored `_device`, `last_affected` and `_new_data_clusters` parameters were dropped. Regression covered by `plan_relocation_handles_growth_that_is_not_a_whole_cluster`, `execute_relocation_round_trips_with_unaligned_growth`, and the end-to-end `test_resize_with_unaligned_fat_growth_preserves_files` (256 MiB, `-s 4`, FAT 1020→1023 — the report's own reproduction). All three were confirmed to fail against the old rounding and pass against the fix.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 960a851

### [F-R1] Resuming from phase `Started` re-runs a non-idempotent data shift over already-shifted data
- **Category:** 19 (lack of idempotency)
- **Impact:** High
- **Explanation:** The single forward pass is safe because moves are sorted highest-cluster-first, but a *re-run* over a device where the pass already progressed reads source sectors the first pass overwrote, and writes that garbage to the new positions.
- **Evidence:** `src/resize/relocator.rs:105` `moves.sort_by(|a, b| b.from_cluster.cmp(&a.from_cluster));` (descending, iterated at `:146`) — so the direction matches TECHGUIDE.md:176 "Why Highest to Lowest?" and a first pass is safe. The destination of cluster *c* is the old home of cluster `c + shift/spc`, which going high→low has already been moved; on a **second** pass `device.read_sectors(mv.from_sector, …)` (`:159`) returns the wrong cluster's data. `src/resize/executor.rs:478` `if starting_phase == ResizePhase::Started {` re-executes the whole shift unconditionally — nothing records how far the previous pass got — and since the boot sector still holds the old `fat_size` until `:578`, `plan_relocation` reproduces an identical plan.
- **Reproduced:** 64 MB filesystem holding 20 MB of files, extended to 128 MB. `FAT32_CRASH_AT=after_data_shift` then a recovery run → recovery exits 0 and logs "Shifted 40962 clusters forward", after which `fsck.fat` reports `Start cluster beyond limit (2313257273 > 258049)`, 31 files instead of 20, only 16 extractable, contents differing. The control run is clean, and `after_checkpoint_start` / `after_checkpoint_data_copied` both recover correctly — isolating the fault to the phase-`Started` resume path.
- **Found by:** Integration & Data
- **Verdict:** CONFIRMED — mechanism established by code reading, matching the empirical reproduction. Triggers whenever `shift < (max_used_cluster − 2) × spc`, i.e. whenever the old and new data ranges overlap.
- **Status:** Fixed
- **Status reason:** The shift now records how far it has got, and a resume continues from that point instead of replaying from the top. `ShiftProgress` (`relocator.rs`) carries a `resume_from` cluster and a `record_every` interval; `execute_relocation` skips the moves above the resume point and durably records the next outstanding cluster as it descends, syncing the preceding batch first so the record never runs ahead of the data. The record is stored in the checkpoint sector at bytes 32..44 (`PRG1` magic, cluster, own CRC32), deliberately *after* the version-1 body so bytes 0..32 stay byte-identical — an older build still reads these checkpoints, and `progress_record_leaves_the_version_1_body_untouched` pins that. Recording every `min(1024, shift_clusters)` moves is what makes the cost negligible: the old home of cluster `c` is not overwritten until the pass descends `shift_clusters` below it, so replaying up to that many moves is provably safe. A checkpoint carrying no progress record is treated as "position unknown" and the resume is refused with `Error::Relocation` rather than guessing — which also covers the sub-cluster-shift geometry (`shift_clusters == 0`), where a cluster overlaps its own source and no move may ever be replayed; such a run still proceeds normally, it simply records nothing and so cannot be resumed. Proven end to end: with the retuned crash-recovery geometry the `after_data_shift` point failed `fsck.fat` before the fix and all 6 crash points pass after it, and at unit level `execute_relocation_resumes_from_the_recorded_point_without_corrupting_data` (crash simulated by failing the progress callback mid-pass) is paired with the negative control `restarting_an_interrupted_shift_from_the_top_corrupts_data`, which fails if the resume point ever stops mattering.
- **Status date:** 2026-07-28 09:55 UTC
- **Status commit:** dc162d7
- **Side effects:** `Error::Relocation` is now constructed, removing one of [F-R33]'s six dead variants. `ShiftProgress` was added to `resize/mod.rs`'s re-exports, since `execute_relocation` is public and would otherwise take an unnameable type — the [F-R44] pattern. Adds two more hand-written byte offsets to the checkpoint codec ([F-R51]), though unlike the existing ones these are covered by tests.
- **Known cost:** progress is recorded at most every `shift_clusters` moves, so a resize whose FAT grows by less than one cluster's worth of sectors degrades to one checkpoint write per moved cluster. Correct, but slow on a large filesystem with a very small shift; buffering that case in memory would be the follow-up.

### [F-R3] TECHGUIDE documents the data shift as idempotent; the code is not, and the test that appears to prove it cannot reach the failing case
- **Category:** 19 (lack of idempotency) + steering-file contradiction
- **Impact:** High
- **Explanation:** The guide's recovery table and "Idempotent Operations" section assert a property [F-R1] shows is false; the premise holds only while the shift distance exceeds the highest in-use cluster's offset.
- **Evidence:** `TECHGUIDE.md:579` `| After data shift | Started | Valid (0xAA55) | Re-run data shift (idempotent) |`; `:596-600` "Source data is **copied**, not moved (original preserved)". The harness that appears to validate this — `scripts/qemu/test-crash-recovery.sh:139-140` `INITIAL_SIZE_MB=64` / `FINAL_SIZE_MB=128` with 20×2KB files plus one 50KB file — applies a ~2034-cluster shift against ~250 used clusters, so the ranges never overlap and the non-idempotency is structurally unreachable.
- **Found by:** Integration & Data
- **Status:** Fixed
- **Status reason:** Both halves addressed. The unreachable test came first, in the safety-net commit (`ff2c929`'s successor `9d778ef`): the crash-recovery harness now writes ~4MB instead of ~90KB, putting the highest in-use cluster near 8100 against a 2046-sector shift, and `assert_overlapping_shift` fails the suite outright if the source and destination ranges ever stop overlapping — turning the silent precondition into a checked one. That restored geometry immediately reproduced [F-R1]. The documentation followed: TECHGUIDE's "Idempotent Operations" section is now "Resumable, Not Idempotent", stating plainly that a second pass corrupts data, showing the concrete sector-level example, and recording that the "copied, not moved" premise holds only while `k` exceeds the highest used cluster. The recovery table's "Re-run data shift (idempotent)" row now reads "Resume shift from recorded progress", matching the mechanism [F-R1] introduced.
- **Status date:** 2026-07-28 09:55 UTC
- **Status commit:** a816370

### [F-R7] Unchecked arithmetic on corruption-controlled values; validation's only cross-field check is computed from the wrapped result
- **Category:** 30 (security as an afterthought)
- **Impact:** High
- **Explanation:** `data_sectors()` subtracts boot-sector-derived quantities from `total_sectors` with bare operators — panic in debug, silent wraparound in release — and the sole FAT32-vs-FAT16 sanity check derives from that wrapped value, so a crafted image passes validation.
- **Evidence:** `src/fat32/structs.rs:395-398` (`data_sectors`), excerpt: `self.total_sectors() as u64 - self.reserved_sectors() as u64 - (self.num_fats() as u64 * self.fat_size() as u64) - root_dir_sectors`; consumed at `src/fat32/validation.rs:116-117` `let cluster_count = boot.data_clusters(); if cluster_count < 65525 {`. Mixed disciplines in the same files: `saturating_sub` at `calculator.rs:99, :126` but bare operators at `:84-87, :106, :117`; `relocator.rs:68` computes `(last_affected - first_affected + 1)` unguarded, and that result feeds `new_first_data_sector` — i.e. where cluster data is physically written.
- **Reproduced:** crafted image with `total_sectors=2000000`, `fat_size=15728640` (FATs larger than the whole volume) → release build reports it as healthy (`Data clusters: 4291285132`); debug build panics `attempt to subtract with overflow` at `structs.rs:395`.
- **Found by:** Security & Code Quality, Error Handling & Observability (merged — R50 folded in during verification)
- **Status:** Fixed
- **Status reason:** `data_sectors()` and `data_clusters()` now return `Option`, computed with `checked_mul`/`checked_add`/`checked_sub` instead of bare operators, so a layout that does not fit in the declared volume yields `None` rather than a panic (debug) or a wrapped value (release). Validation gained an explicit cross-field check that consumes that `Option` and is placed **before** the FAT32-vs-FAT16 cluster-count test — the test that previously read the wrapped result. The two failure modes are now one clear error naming the actual arithmetic: `"Filesystem layout does not fit in the volume: 32 reserved sectors + 2 FATs x 15728640 sectors exceeds 2000000 total sectors"`. The report's own reproduction was re-run against both profiles: the release build previously reported the crafted image as healthy with `Data clusters: 4291285132` and now exits 1 with that message; the debug build previously panicked `attempt to subtract with overflow` at `structs.rs:395` and now produces the identical error. `BootSector`'s `Debug` impl also calls `data_clusters()`, so debug-printing a corrupt boot sector panicked too — that is covered by `debug_formatting_survives_a_corrupt_boot_sector`. The ten call sites were each given a considered treatment rather than a blanket unwrap: `calculate_new_size`, `plan_relocation`, the checkpoint-resume `SizeCalculation` and `get_fs_info` propagate a typed error; `find_free_cluster` (dead per [F-R34]) returns `None`; verbose output renders `<layout does not fit in the volume>`; and the FSInfo free-count site falls back to a zero delta — see the caveat below. Boundary behaviour is pinned by `data_area_boundary_is_exact`: a layout exactly filling the volume is `Some(0)`, one sector further is `None`.
- **Status date:** 2026-07-28 14:55 UTC
- **Status commit:** f719f90
- **Caveat:** the FSInfo free-count update at `executor.rs` uses `.unwrap_or(calculation.new_data_clusters)`, yielding a zero delta rather than propagating. The `None` is unreachable there (the old geometry came off a filesystem that validated), and the site sits mid-danger-zone where erroring would abandon a partially-resized filesystem — but it is a silent fallback in code that writes metadata, which is the same shape as [F-R45].

### [F-R5] CLI decides whether to attempt crash recovery by substring-matching a formatted error message
- **Category:** 20 (weak error handling strategy) / 13 / 24
- **Impact:** High
- **Explanation:** Crash-recovery entry depends on `format!("{:?}", e)` containing a literal produced by an unrelated `format!` in the validator; rewording either string silently turns recovery off and leaves users with an unrecoverable filesystem.
- **Evidence:** `src/main.rs:182-183`, excerpt: `let err_msg = format!("{:?}", e); if err_msg.contains("Invalid boot signature: 0x0000") {`; producing side `src/fat32/validation.rs:24-27` `Error::BootSectorValidation(format!("Invalid boot signature: {:#06X} (expected 0xAA55)", sig))`. `Error::InvalidatedFilesystem` exists (`error.rs:62-63`) but is constructed only inside the executor and never surfaced by `get_fs_info`. Root cause: `get_fs_info` calls the strict `read_boot_sector` (`executor.rs:711`) while `resize_fat32` calls `read_boot_sector_for_recovery` (`:342`), so the CLI must reverse-engineer the reason.
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Error Handling & Observability, Integration & Data (4 specialists)
- **Note (2026-07-28, fix session):** Validated as still present and fully covered by safety-net tests (commit `5fe2168`), but **not fixed** — a design decision is outstanding. Two approaches were scoped:
  - *Typed variant:* add `Error::BootSectorInvalidated`, returned by `validate_boot_sector_impl` when `sig == 0x0000 && !allow_invalidated`, and matched in `main.rs` instead of the substring. Small, localised, no API change. Note that reusing the existing `Error::InvalidatedFilesystem` is **wrong**: the executor constructs it at `executor.rs:314,321` to mean recovery has already *failed*, while the CLI needs "this might be recoverable, try" — one variant for both would move F-R5's conflation from strings into the type system, where it would look correct.
  - *Recovery-aware query:* have `get_fs_info` report the invalidated state as data so the CLI never interprets a failure. This **conflicts with an existing assertion**: `invalidated_boot_sector_without_a_checkpoint_is_not_recoverable` (`tests/integration_test.rs`) asserts `get_fs_info(...).is_err()` on an invalidated boot sector, and a recovery-aware query would succeed instead. That is a behaviour change requiring an explicit decision, not a test edit.
  - Also discovered while writing the safety net: the CLI's recovery dispatch is **narrower than this entry describes**. `read_boot_sector` checks the declared sector size before validation reaches the signature, so a badly-mangled boot sector fails with `UnsupportedSectorSize` and never reaches the recovery decision at all. Only a boot sector plausible in every respect *except* its signature gets there. A fix should not assume "invalidated" and "unreadable" are the only two cases.
- **Status:** Fixed
- **Status reason:** The *typed variant* approach, chosen by the developer. `Error::BootSectorInvalidated` is returned by `validate_boot_sector_impl` when the signature is `0x0000` and `allow_invalidated` is false, and `main.rs` matches on it instead of on `format!("{:?}", e).contains("Invalid boot signature: 0x0000")`. The producing `format!` remains only for genuinely-corrupt signatures, where nothing consumes it as a string — so the two sites now agree through a type the compiler checks rather than through matching text. `Error::InvalidatedFilesystem` was deliberately not reused: the executor constructs it to mean recovery has already *failed*, and merging the two would have restated this flaw's conflation in the type system, where it would look correct. Zero safety-net tests needed editing, which was the signal the fix was correctly sized: the existing assertions are variant-agnostic (`is_err()`), and `test_invalid_boot_signature` sets only `data[510]`, leaving signature `0xAA00` — garbage, not the invalidation marker — so it keeps asserting `BootSectorValidation`. Verified: 75 unit + 8 error-contract + 5 fsinfo + 12 integration all pass (including `invalidated_boot_sector_without_a_checkpoint_is_not_recoverable`, whose contract is unchanged), `cargo fmt --check` and `clippy -D warnings` clean.
- **Status date:** 2026-07-28 18:20 UTC
- **Status commit:** ac477aa
- **Resolved by the developer (2026-07-28):** the narrow recovery gate described in the third bullet above is **working as designed, not a defect**: the tool protects against *its own process dying*, not against arbitrary damage to a filesystem it never touched. A boot sector mangled beyond its signature should fail rather than have the resize machinery driven over it. This rationale is now recorded as a comment at the dispatch site in `main.rs` so it is not "fixed" by a later reader. No new finding was logged.

### [F-R9] The crash-recovery test suite is never executed by CI
- **Category:** 32 (missing test coverage for critical paths)
- **Impact:** High
- **Explanation:** Test discovery globs one directory that `test-crash-recovery.sh` does not live in, and CI never builds the `fault-injection` feature, so the only test of the checkpoint/resume machinery runs solely when a human invokes it by hand.
- **Evidence:** `scripts/qemu/test-qemu.sh:404`, excerpt: `mapfile -t tests < <(find "$SCRIPT_DIR/tests" -name "test-*.sh" -type f | sort)`. `scripts/qemu/tests/` contains 9 files; `test-crash-recovery.sh` sits one level up in `scripts/qemu/`. `.github/workflows/tests.yml` invokes `./scripts/qemu/test-qemu.sh` with no arguments, and the only `cargo build --features fault-injection` in the repo is inside `test-crash-recovery.sh:57`.
- **Found by:** Security & Code Quality
- **Note:** This is the direct reason [F-R1] survived to the current commit. Qualifies [S-D].
- **Status:** Fixed
- **Status reason:** `.github/workflows/tests.yml` gained a "Run crash recovery tests" step that invokes `./scripts/qemu/test-crash-recovery.sh` after the QEMU step, with pass/fail reported into the job summary. No `fault-injection` build step was needed in the workflow — the script already builds its own binary with that feature (`:57-58`), and the Alpine setup and musl target CI needs are steps that already existed. The script was deliberately *not* moved into `scripts/qemu/tests/`: it drives its VM differently from the nine discovered suites, and restructuring a working crash-safety harness carried more risk than the duplication saves. Instead the glob at `test-qemu.sh:404` now carries a comment stating that discovery covers `tests/` only, that this suite is invoked separately by CI, and that adding a suite outside `tests/` without a matching workflow step means it never runs anywhere — which is precisely how this flaw arose. Verified by running the new step's exact command and summary parsing locally (exit 0, 6 crash points parsed) and confirming `test-qemu.sh` still discovers its 9 tests.
- **Status date:** 2026-07-28 09:55 UTC
- **Status commit:** b0f04c7

### [F-R61] Three integration test binaries are never executed by CI
- **Category:** 32 (missing test coverage for critical paths)
- **Impact:** Medium
- **Explanation:** The same defect class as [F-R9], in a different place. `.github/workflows/tests.yml` runs `cargo test --lib` and `cargo test --test integration_test -- --ignored`, naming exactly two targets. `tests/error_contract_test.rs`, `tests/fsinfo_report_test.rs` and `tests/public_api_test.rs` — 24 tests, several of them written as safety nets by earlier fix sessions in this very report ([F-R31], [F-R44]) — are in neither, so they run only on developers' machines.
- **Evidence:** `.github/workflows/tests.yml:53` `cargo test --lib`, `:69` `cargo test --test integration_test -- --ignored`. Against `ls tests/` → `error_contract_test.rs`, `fsinfo_report_test.rs`, `integration_test.rs`, `public_api_test.rs`. A safety net CI does not run cannot fail a pull request, which is the only thing that makes it a net.
- **Found by:** 2026-07-31 fix session, while wiring the new `durability_test` target into CI
- **Suggested fix:** replace the two named targets with `cargo test --all-targets` plus a `-- --ignored` pass, so a new test file is covered the day it is added rather than the day someone remembers to name it. Note that this will surface any currently-failing test in those three files for the first time.
- **Status:** Fixed
- **Status reason:** The two steps now sweep with `cargo test --all-targets` and `cargo test --all-targets -- --ignored` instead of naming targets, so a new `tests/*.rs` file is covered the day it is written. The durability step keeps its explicit `--features device-journal --test durability_test` invocation, which a sweep cannot replace: without the feature that suite compiles to zero tests and a plain `--all-targets` run reports `ok` while asserting nothing. The suggested fix warned this might surface previously-unrun failures — it did not: all six binaries were already green locally (126 default + 16 ignored, 0 failures), verified before the workflow was touched. New `tests/ci_workflow_test.rs` fails the build if CI ever regresses to named targets, and separately if the durability invocation loses its feature flag.
- **Status date:** 2026-08-02 08:20 UTC
- **Status commit:** 155f7b6

### [F-R10] Relocation sector math is untested; the unit tests only assert on hand-built structs
- **Category:** 32 (missing test coverage for critical paths)
- **Impact:** High
- **Explanation:** No test calls `plan_relocation` or `execute_relocation`; all four tests construct `ClusterMove`/`RelocationPlan` literals and read the fields back, so they would pass unchanged if the sector arithmetic were inverted.
- **Evidence:** `src/resize/relocator.rs:187-256` — tests are `test_cluster_move` (`:191`), `test_relocation_plan_empty` (`:206`), `test_relocation_plan_with_moves` (`:219`), `test_verify_relocation_always_succeeds` (`:246`). Only the last calls a production function, and that function is a stub ([F-R20]). CLAUDE.md and TECHGUIDE both name the old-vs-new `first_data_sector` distinction as the project's critical insight — and [F-R2] lives in exactly this untested code.
- **Found by:** Security & Code Quality, Coupling & Dependencies
- **Status:** Fixed
- **Status reason:** Six tests added that call `plan_relocation`/`execute_relocation` against a temp-file-backed `Device`: data-area start vs. the grown boot sector, per-cluster source/target sectors, in-use filtering, descending move order, byte-level round trip, and preservation under an overlapping shift in a single pass. The four pre-existing struct-literal tests were left in place. Verified to catch [F-R2] by reintroducing the old rounding and observing the failure.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** ff2c929

### [F-R4] `--force` is advertised for backup-boot-sector mismatch but is never plumbed into the library
- **Category:** 24 (inconsistent API contracts)
- **Impact:** Medium *(corrected down from High: the failure is fail-closed — the tool refuses rather than proceeding unsafely; the defect is a false promise, not unsafe behaviour)*
- **Explanation:** The CLI prints "Use --force to proceed anyway", but `ResizeOptions` carries no force field, so `resize_fat32` re-checks and returns `Error::BackupMismatch` regardless.
- **Evidence:** `src/main.rs:174` `anyhow::bail!("Use --force to proceed anyway");` against `src/resize/executor.rs:259-263` `pub struct ResizeOptions { device_path: …, dry_run: bool, verbose: bool }` and `:375-377` `if boot.is_signature_valid() && !boot_sectors_match(&boot, &backup_boot) { return Err(Error::BackupMismatch); }`. Verified empirically: corrupting sector 6 then running `resize --force` → exit 1, "Backup boot sector does not match primary boot sector".
- **Found by:** Error Handling & Observability, Integration & Data, Structure & Boundaries
- **Status:** Fixed
- **Status reason:** `ResizeOptions` gains `ignore_backup_mismatch` (builder + `ignores_backup_mismatch()` getter), consulted at the `boot_sectors_match` check in `executor.rs`; the CLI flag is renamed `--force` → `--ignore-backup-mismatch` and actually wired into the options struct it previously never reached. Fixed jointly with [F-R46], because the two were one design decision split across two entries: as long as `--force` also disabled the privilege gate, giving it teeth on the mismatch path would have meant one flag silently authorising two unrelated risks. The override is deliberately narrow and *not* a general "force" — a mismatch means the two copies of the geometry disagree, and the resize resolves it by trusting the primary and rewriting the backup from it, so if the primary is the damaged copy the override destroys the only good record of the original layout. It is therefore an assertion by the caller that the primary is correct, and it is recorded as such in the operations log rather than passing silently. Refusal remains the default at both layers. This does **not** close the [F-R26] correction's larger point: making a valid primary *authoritative* over a differing backup is a separate change, still open. Verified as for [F-R46].
- **Status date:** 2026-08-01 12:34 UTC
- **Status commit:** d78205f

### [F-R6] Attacker-controlled allocation size from on-disk `fat_size_32` → process abort
- **Category:** 30 (security as an afterthought)
- **Impact:** Medium *(corrected down from High: the process aborts before any write — a self-DoS on a CLI tool, not corruption; [F-R7], sharing the root cause, carries the High)*
- **Explanation:** `read_fat_table` sizes two `Vec`s from `fat_size × bytes_per_sector`, values validation only checks for `!= 0`, never against device size or total sectors.
- **Evidence:** `src/fat32/operations.rs:102-103` (`read_fat_table`), excerpt: `let total_bytes = fat_sectors as usize * bytes_per_sector; let mut fat_data = Vec::with_capacity(total_bytes);`; the only check on the field is `validation.rs:94-96` `if boot.fat_size_32() == 0 {`.
- **Reproduced:** crafted 1.2 GB image with `bytes_per_sector=4096`, `fat_size_32=0xF0000000` passes validation and reaches the allocation → `memory allocation of 16492674416640 bytes failed`, core dumped, exit 134.
- **Found by:** Security & Code Quality
- **Status:** Partially fixed
- **Status reason:** [F-R7]'s cross-field layout check kills the recorded reproduction — a boot sector declaring `fat_size_32=0xF0000000` with `bytes_per_sector=4096` is now rejected by `validate_boot_sector` before `read_fat_table` is ever reached, on both the `info` and `resize` paths (re-run and confirmed against the fixed release build). The allocation is consequently bounded by the declared volume size instead of by `u32::MAX`. It is **not** bounded by anything sane, so the flaw stands in reduced form: a *self-consistent* geometry still passes. A crafted image declaring `total_sectors=4_000_000_000` and `fat_size=1_000_000_000` clears the layout check (2×10⁹ + 32 ≤ 4×10⁹) and would ask `read_fat_table` for a 512 GB allocation; in the test above it stopped only at the CLI's "already at maximum size" guard, which fired because the backing file was 4 MB. On a real device large enough for the declared geometry — a 1 TB disk is ≈2×10⁹ sectors — the allocation is still reachable. Closing this needs a bound relating `fat_size` to the cluster count it has to describe (`fat_size ≈ data_clusters × 4 / bytes_per_sector`, within tolerance), rather than merely "the FATs fit in the volume". [F-R8]'s device-bounds check would narrow it further but would not close it either, since a large real device admits a large declared `fat_size`.
- **Status date:** 2026-07-28 15:20 UTC
- **Status commit:** f719f90 (partial, via [F-R7])

### [F-R8] Validation checks fields individually, never cross-field and never against the actual device
- **Category:** 30 (security as an afterthought)
- **Impact:** Medium *(corrected down from High: the concrete exploits are counted as [F-R6]/[F-R7]; this is the structural gap behind them)*
- **Explanation:** Every check is a per-field range test; nothing verifies the layout fits inside `total_sectors_32`, that `total_sectors_32` fits the opened device, or that `root_cluster` is within the cluster count.
- **Evidence:** `src/fat32/validation.rs:5` `fn validate_boot_sector_impl(boot: &BootSector, allow_invalidated: bool)` — the signature takes no `&Device`, so the device-bounds check is structurally impossible here. The nearest cross-field check is `:116-122`, and per [F-R7] it consumes a value that can wrap.
- **Found by:** Security & Code Quality
- **Qualifies:** [S-H].

### [F-R11] No tests for checkpoint serialization, CRC corruption, resume logic, or FAT1→FAT2 sync
- **Category:** 32 (missing test coverage for critical paths)
- **Impact:** Medium *(corrected down from High; [F-R10] is the High test-coverage finding)*
- **Explanation:** The complete test module is two struct-field assertions; nothing exercises the crash-safety machinery that is the crate's main claim.
- **Evidence:** `src/resize/executor.rs:844-885` — `test_resize_options` (`:848`) and `test_resize_result` (`:857`) only. Untested: `to_bytes`/`from_bytes` round-trip, `Error::CheckpointCorrupted` on a flipped CRC bit (`:171-175`), `ResizePhase::from_u8` rejecting out-of-range, the phase-skipping resume branches, `sync_fat_copies`, `init_new_fat_sectors` — all straightforwardly testable against a tempfile image (see [S-Q]).
- **Found by:** Security & Code Quality

### [F-R12] No negative or fuzz testing of the untrusted-input parser
- **Category:** 32 (missing test coverage for critical paths)
- **Impact:** Medium *(corrected down from High)*
- **Explanation:** Three rejection cases cover roughly a dozen checks, and there is no fuzzing or property testing of the on-disk parser at all.
- **Evidence:** `src/fat32/validation.rs:181-297` — `test_invalid_boot_signature` (`:260`), `test_not_fat32` (`:269`), `test_invalid_fsinfo` (`:291`), each built from `create_valid_fat32_boot_sector()` with at most one byte pair altered. No `fuzz/` directory; `grep 'proptest\|quickcheck\|arbitrary\|fuzz' Cargo.toml Makefile` → no hits. [F-R6] and [F-R7] are both reachable through inputs no existing test resembles.
- **Found by:** Security & Code Quality

### [F-R13] Layout arithmetic duplicated across executor, calculator and BootSector, with a comment admitting the copies must be hand-synced
- **Category:** 9 (shotgun surgery)
- **Impact:** Medium
- **Explanation:** The data-cluster formula exists in four places and the affected-cluster formula in two; one inline copy silently omits the `root_dir_sectors` term the canonical accessor includes.
- **Evidence:** `src/resize/executor.rs:410` `// Note: formula matches calculator.rs: last = first + affected_clusters - 1`; `:591-594` `(calculation.old_total_sectors - boot.reserved_sectors() as u32 - (boot.num_fats() as u32 * calculation.old_fat_size)) / boot.sectors_per_cluster() as u32` (no `root_dir_sectors` term); plus `:651-654` `calculate_data_clusters_from_params`, `calculator.rs:84-87`, and the canonical `structs.rs:391-404`.
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Integration & Data (3 specialists)
- **Note:** [F-R2] is a direct consequence of this class of divergence. Cross-references [F-R45].
- **Status:** Fixed
- **Status reason:** Both duplicated formulas now have a single home. `BootSector::with_geometry(total_sectors, fat_size)` returns the boot sector as it would look under a different geometry, so every layout question is answered by the canonical accessors; the three data-cluster copies (`calculator.rs:84-87`, the inline FSInfo copy at `executor.rs:591-594` which omitted `root_dir_sectors`, and `calculate_data_clusters_from_params`) are replaced by `with_geometry(..).data_clusters()`, and the last of those functions is deleted. The affected-cluster formula, duplicated between `calculator.rs` and the checkpoint-resume branch at `executor.rs:409-417` (with the comment admitting the copies had to be hand-synced), is now `calculator::affected_cluster_range`, called from both. The commit path at `executor.rs:566-567` also goes through `with_geometry`, so the projected and committed boot sectors are built by the same code. Covered by `test_data_clusters_for_a_projected_geometry` and `test_with_geometry_preserves_the_rest_of_the_sector`; the former computes its expected values independently rather than comparing two implementations against each other.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 23fdf7a

### [F-R14] `resize_fat32` is a ~307-line function doing everything
- **Category:** 2 (god object)
- **Impact:** Medium *(corrected down from High: size alone; the correctness consequences are counted as [F-R2]/[F-R13]/[F-R15])*
- **Explanation:** One function performs mount checking, device opening, boot-sector reading, crash detection, size calculation, FAT loading, three-phase execution, boot/backup/FSInfo updates and operation-log assembly.
- **Evidence:** `src/resize/executor.rs:321-627` `pub fn resize_fat32(options: ResizeOptions) -> Result<ResizeResult>`. Nesting reaches five block levels at `:476-529` (`fn` → `if fat_needs_growth` → `if !plan.is_empty()` → `if !is_dry_run()` → `if starting_phase == Started`). Dry-run is re-tested at `:329`, `:349`, `:476`, `:574`; an inline `SizeCalculation { … }` literal at `:392-418` duplicates `calculator.rs`.
- **Found by:** Structure & Boundaries

### [F-R15] Resize phase ordering is enforced only by an integer enum and comments
- **Category:** 27 (temporal coupling)
- **Impact:** Medium *(corrected down from High — see the correction below)*
- **Explanation:** The crash-safe sequence lives in one flat function guarded by `if starting_phase == …` / `<= …` and `// === DANGER ZONE START/END ===` comments; nothing in the type system prevents skipping, reordering or re-entering a phase, and "boot sector currently invalidated" is represented nowhere but a mutable local.
- **Evidence:** Brace structure traced explicitly: `if calculation.fat_needs_growth {` (`:448`) → `if !plan.is_empty() {` (`:464`) → `if !options.is_dry_run() {` (`:476`) containing `if starting_phase == ResizePhase::Started {` (`:478-526`) and `if starting_phase <= ResizePhase::DataCopied {` (`:532-564`), all closing at `:571-572`.
- **Found by:** Coupling & Dependencies
- **Correction applied during verification:** the original claim was that `boot.restore_signature();` at `:580` sits outside both phase guards and therefore runs regardless of which phase invalidated the signature. The placement is confirmed exactly as described — lines 574-590 are inside only `if !options.is_dry_run() {` — but it is **benign**, not a defect: restoring an already-valid signature is idempotent, and every reachable path (fresh run, resume from `DataCopied`, resume from `FatWritten`, no-FAT-growth) wants signature-valid as the end state. The surviving finding is the absence of type-level phase enforcement, not a bug at `:580`.

### [F-R16] `init_new_fat_sectors` doc states the opposite of the required order
- **Category:** 34 (inconsistent conventions / misleading documentation)
- **Impact:** Medium
- **Explanation:** The function that zeroes the FAT-extension sectors documents itself as running before relocation, but it is called after — and must be, since it overwrites the sectors whose cluster data relocation just moved forward.
- **Evidence:** `src/resize/executor.rs:658`, excerpt: `/// This must be called BEFORE relocation so that reading new FAT sectors returns valid data`; actual order is `execute_relocation(…)` at `:493`, then `init_new_fat_sectors(&device, &boot, &calculation)?` at `:543`. The neighbouring `sync_fat_copies` doc at `:681` ("must be called AFTER relocation") is correct, which makes the wrong one easy to trust.
- **Found by:** Coupling & Dependencies, Error Handling & Observability
- **Status:** Fixed
- **Status reason:** The doc now states the actual constraint (AFTER relocation) together with the reason it cannot be otherwise: the sectors it zeroes are FAT1's extension, which is physically the region relocation has just vacated, so running it first would erase live cluster data that relocation then copies forward from an already-blank area. The inverted claim was the whole defect — the call site was always correct — so no code changed. Cross-referenced to [F-R15]: nothing but the call site enforces the order.
- **Status date:** 2026-08-02 08:35 UTC
- **Status commit:** _(recorded at the end of the session)_

### [F-R17] No durability barrier between the FAT writes and the phase-2 checkpoint
- **Category:** 26 (poor transactional boundaries)
- **Impact:** Medium *(corrected down from High: requires power loss inside one flush window)*
- **Explanation:** `init_new_fat_sectors` and `sync_fat_copies` issue hundreds of unsynced sector writes, then `write_checkpoint(FatWritten)` writes and syncs — one flush covering both, so the device may commit the checkpoint before the FAT sectors, after which recovery skips the FAT work.
- **Evidence:** The crate has exactly four `sync()` call sites — `relocator.rs:166`, `executor.rs:208` (inside `write_checkpoint`), `:537`, `:613`. Neither `init_new_fat_sectors` (`:659-678`) nor `sync_fat_copies` (`:682-705`) contains one; contrast `:536-537` `write_boot_sector(&device, &boot)?; device.sync()?;`. Recovery skips at `:565-567` `operations.push("Skipping FAT operations (already done)".to_string());`.
- **Found by:** Integration & Data
- **Qualifies:** [S-C].
- **Status:** Fixed
- **Status reason:** A `device.sync()?` now sits between `sync_fat_copies` and the phase-2 `write_checkpoint`, so the FAT extension is on the platter before the record claiming it is durable. One barrier covers both FAT helpers rather than one each: nothing reads the device between `init_new_fat_sectors` and `sync_fat_copies`, so a second flush would buy no guarantee. The audit trail gains a matching "Flushed FAT writes" entry, since a barrier that leaves no record is indistinguishable from a missing one in a post-mortem.
- **Note on why this needed new test infrastructure:** the flaw was untestable by every harness the project had. `sync()` returns nothing, and the QEMU crash harness kills the process — after which the page cache flushes everything regardless of what the code asked for, so it cannot reproduce the reordering this is about. A `device-journal` cargo feature (off by default, mirroring `fault-injection`) makes `Device` record each sector write and each sync into a thread-local journal, which is what turns the barrier into an assertable fact. Covered by `the_fat_extension_is_flushed_before_the_phase_2_checkpoint`, which was confirmed red first — the window between the last FAT write and the checkpoint write contained *no operations at all*.
- **Safety-net integrity:** no safety-net test was edited. `a_fat_growth_resize_writes_the_fat_extension_before_the_phase_2_checkpoint`, which pins the write *ordering* the fix must not disturb, passed unchanged.
- **Status date:** 2026-07-31 00:00 UTC
- **Status commit:** e19a9fb

### [F-R18] Commit phase has no barrier between restoring the boot sector and clearing the checkpoint
- **Category:** 26 (poor transactional boundaries)
- **Impact:** Medium *(corrected down from High, same reasoning as [F-R17])*
- **Explanation:** Boot sector, backup boot sector, FSInfo and the checkpoint erase are all written unsynced and flushed by a single `device.sync()` at the end; if the erase lands first and power is lost, the device has an invalidated signature and no checkpoint, which the tool's own recovery treats as unrecoverable.
- **Evidence:** `src/resize/executor.rs:582` `write_boot_sector`, `:586` `write_backup_boot_sector`, `:605` `write_fsinfo`, `:609` `clear_checkpoint` (which, unlike `write_checkpoint` at `:204-210`, contains no `sync()` — see `:225-230`), then the only flush at `:613` `device.sync()?;`. Dead end at `:249` `None => Err(Error::InvalidatedFilesystem),`.
- **Found by:** Integration & Data
- **Qualifies:** [S-C].
- **Status:** Fixed
- **Status reason:** A `device.sync()?` now sits between `write_fsinfo` and `clear_checkpoint`, so the restored boot sector, the backup boot sector and FSInfo are all durable before the record that protects them is retired. The trailing `device.sync()` at the end of the commit phase is kept: it flushes the erase itself, which the new barrier does not cover. The audit trail gains a "Flushed new geometry" entry for the same reason as [F-R17]'s — a barrier that leaves no record cannot be distinguished from a missing one after the fact.
- **On the asymmetry with [F-R17]:** this barrier is placed before the *erase*, not before each write, because the ordering that matters is one-to-many: boot, backup and FSInfo may reach the platter in any order relative to each other without harm, but none may follow the erase. A single flush before `clear_checkpoint` expresses exactly that and costs one extra sync per resize.
- **Covered by:** `the_new_geometry_is_flushed_before_the_checkpoint_is_cleared`, confirmed red first — like [F-R17], the window between the FSInfo write and the checkpoint erase contained no operations at all.
- **Safety-net integrity:** no safety-net test was edited. `the_commit_phase_writes_boot_backup_and_fsinfo_before_clearing_the_checkpoint`, which pins the commit-phase write order and the trailing flush, passed unchanged.
- **Status date:** 2026-07-31 00:00 UTC
- **Status commit:** 350bd8f

### [F-R19] The entire audit trail is discarded on the failure path
- **Category:** 21 (no observability plan)
- **Impact:** Medium *(corrected down from High)*
- **Explanation:** `operations` accumulates a record of every device mutation but is surfaced only by being moved into `ResizeResult` on success; every `?` drops it, so a failure inside the danger zone leaves a one-line error and zero record of which of ~15 mutations already happened.
- **Evidence:** `src/resize/executor.rs:322` `let mut operations = Vec::new();`; the sole read is the struct literal at `:625`. The vector appears in no error construction anywhere in the crate.
- **Found by:** Error Handling & Observability
- **Qualifies:** [S-I].

### [F-R20] "Verified data shift" is logged for a verification that does nothing
- **Category:** 21 (no observability plan) / 31 (dead code)
- **Impact:** Medium *(corrected down from High: misleading, not destructive)*
- **Explanation:** `verify_relocation` ignores all three parameters and returns `Ok(())`, yet the executor pushes an audit entry asserting a safety check ran — on the one operation that physically moves user data.
- **Evidence:** `src/resize/relocator.rs:178-185`, excerpt: `pub fn verify_relocation(_fat: &[u32], _first_affected: u32, _last_affected: u32) -> Result<()> { … // For API compatibility, we just return Ok. Ok(()) }`; `src/resize/executor.rs:505-510` `verify_relocation(…)?; operations.push("Verified data shift".to_string());`. Its own unit test is named `test_verify_relocation_always_succeeds` (`relocator.rs:247`).
- **Found by:** all five specialists — the highest corroboration in the set
- **Status:** Fixed
- **Status reason:** `verify_relocation` is deleted, along with its call site, its `resize/mod.rs` re-export and the `"Verified data shift"` audit entry it justified. The audit trail no longer claims a check that never ran. Implementing a real verification was considered and rejected: a read-back comparison would double read I/O on the slowest phase and could not catch a misdirected write — it would re-read from the same wrong place and pass — while the structural invariants worth asserting are already guaranteed by `plan_relocation` deriving its geometry from the boot sector that will be committed ([F-R2]'s fix). What the stub pretended to check is now genuinely covered: relocation correctness by the unit tests in `relocator.rs`, and end-to-end integrity by the crash-recovery suite that [F-R9] wired into CI. Its own test, `test_verify_relocation_always_succeeds`, is deleted with it — it asserted the do-nothing behaviour, so it pinned the flaw rather than guarding against it.
- **Status date:** 2026-07-28 09:55 UTC
- **Status commit:** 614ba58

### [F-R21] No logging facade and no durable log; raw `eprintln!` inside library business logic
- **Category:** 21 (no observability plan)
- **Impact:** Medium
- **Explanation:** For a tool that can destroy a filesystem, the only evidence after an unattended failure is whatever the terminal captured; an embedder cannot capture, suppress or redirect output.
- **Evidence:** 26 `eprintln!` calls in `src/`, 18 of them in `src/resize/`; `grep 'log\|tracing' Cargo.toml` → no logging dependency. `verbose` is threaded as a bare `bool` through `execute_relocation(…, verbose: bool)` (`relocator.rs:132`). The only durable artifact is the on-device checkpoint, which is erased on success.
- **Found by:** Error Handling & Observability, Coupling & Dependencies

### [F-R23] `Device` is two-phase-initialised with a silently wrong default sector size
- **Category:** 27 (temporal coupling)
- **Impact:** Medium
- **Explanation:** `Device::open` hardcodes 512 and computes `total_sectors` from it; every sector-addressed read/write is wrong until `set_sector_size` is called, which happens only as a side effect of reading the boot sector — a caller touching sectors before that gets silently misaddressed I/O, not an error.
- **Evidence:** `src/device.rs:48-51`, excerpt: `// Default to 512-byte sectors (most common) // We'll update this after reading the boot sector` … `let sector_size = 512u32; let total_sectors = size / sector_size as u64;`; obligation discharged inside an unrelated function at `src/fat32/operations.rs:33` `device.set_sector_size(sector_size as u32);`. Consequence: `write_checkpoint`/`read_checkpoint`/`clear_checkpoint` all address `device.total_sectors() - 1` (`executor.rs:205, 219, 226`), so the checkpoint's location depends on whether that hidden initialisation ran.
- **Found by:** Coupling & Dependencies, Structure & Boundaries, Error Handling & Observability
- **Status:** Fixed
- **Status reason:** `Device` no longer has a default sector size or a two-phase init. `Device::open_with(path, writable, sector_size_from)` determines the geometry before `Self` exists: the caller supplies a probe closure that turns the leading bytes into a sector size, so `device` stays format-agnostic while `fat32` supplies the FAT knowledge (`fat32::operations::bootstrap_sector_size`, wrapped as `open_fat32_device`). `set_sector_size` and the hardcoded `512` are deleted, so the window in which a `Device` addressed sectors at the wrong size is not merely closed but unrepresentable. The file is still opened exactly once. Both production call sites (`resize::fsinfo::get_fs_info`, `resize::executor::resize_fat32`) now go through `open_fat32_device`. Covered by `total_sectors_always_matches_the_device_size_divided_by_the_sector_size`, `a_sector_index_addresses_sector_size_times_index_bytes`, `checkpoint_round_trips_at_a_non_default_sector_size`, `crash_detection_finds_a_checkpoint_at_a_non_default_sector_size`, `device_geometry_matches_a_4096_byte_sector_filesystem` and `resize_preserves_files_on_a_4096_byte_sector_filesystem`.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 7d278d9

### [F-R25] FAT extension is guarded by `!plan.is_empty()` but the metadata commit is unconditional
- **Category:** 26 (poor transactional boundaries)
- **Impact:** Medium
- **Explanation:** If the FAT must grow but no cluster is in use, the tool publishes new FAT geometry over an unwritten FAT and a FAT2 still at its old offset.
- **Evidence:** `src/resize/executor.rs:464` `if !plan.is_empty() {` closes at `:571`; `:574` `if !options.is_dry_run() {` then runs `:579` `boot.set_fat_size_32(calculation.new_fat_size);`.
- **Found by:** Integration & Data
- **Verdict:** CONFIRMED (latent) — a mkfs'd root directory always occupies cluster 2, so the plan is never empty in practice.

### [F-R26] The no-FAT-growth path and the whole metadata phase are outside checkpoint protection
- **Category:** 26 (poor transactional boundaries)
- **Impact:** Medium
- **Explanation:** No checkpoint is ever written when the FAT does not grow, and even in the growth case the final metadata phase has none of its own, so a crash after the boot sector is committed is unresumable.
- **Evidence:** `src/resize/executor.rs:448` `if calculation.fat_needs_growth {` encloses every `write_checkpoint` site (`:487, :522, :560`); `read_checkpoint:215-217` `if device.total_sectors() <= boot.total_sectors() as u64 { return Ok(None); }` refuses to look once the filesystem covers the device. Verified: re-running afterwards yields exit 1 "already at maximum size", leaving a stale valid checkpoint record inside the new free space.
- **Found by:** Integration & Data
- **Status:** Partially fixed
- **Status reason:** The metadata phase now has a single commit point instead of none. The primary boot sector is written **last**, after the backup boot sector and FSInfo have been written and flushed, so a crash before it lands leaves a filesystem that still reads as its old, coherent self, and a crash after it lands leaves one whose supporting structures are already correct. Previously the primary went first, which published a new geometry over structures that had not been written yet. Covered by `the_backup_and_fsinfo_are_durable_before_the_primary_boot_sector_commits`.
- **The first half may be moot — see [F-R56]:** validation later in the same session found no reachable input that takes the no-FAT-growth branch at all (ten increments from +4 KB to +22 MB all grew the FAT). If that holds, "no checkpoint is written when the FAT does not grow" describes dead code rather than a live gap, and this finding's evidence — which treats 128 MB → 150 MB as the no-growth case — is measurably wrong about that resize. Resolving F-R26 fully should start by settling whether the branch is reachable.
- **What this does NOT fix, stated precisely:** the finding's first half — no checkpoint is written at all when the FAT does not grow — **still stands**. The reorder makes the metadata phase atomic by ordering rather than by checkpoint, which is why it needed no change to `read_checkpoint`'s "filesystem fills the device" guard, but it adds no checkpoint anywhere.
- **Correction to this session's own framing:** the approach was chosen partly on the argument that it would shrink the `BackupMismatch` window on the no-growth path. On closer analysis that is **wrong, and worth recording so it is not repeated**. Before, a crash between the two writes left primary=new/backup=old; after, it leaves primary=old/backup=new. Both are mismatches and the window is the same size. What actually improves is *which* state survives — the new order leaves the old, internally consistent geometry rather than a new geometry over unwritten structures — not the odds of landing in it. The dead end itself is untouched: `Error::BackupMismatch` is still fatal and `--force` is still not plumbed into the library ([F-R4]), so a filesystem caught in that window cannot be recovered by this tool. **Closing F-R26 properly requires [F-R4], and probably making a valid primary authoritative over a differing backup.**
- **Coverage caveat:** the QEMU crash-recovery suite (`scripts/qemu/test-crash-recovery.sh`), which exercises recovery from all six fault-injection points and is the only end-to-end test of the crash protocol this change alters, was **not run** for this fix — offered and declined in favour of speed. The Rust suite, the 15 integration tests and `scripts/test-resize.sh` all pass. Anyone revisiting this should run the QEMU suite before trusting the reorder under real crash conditions.
- **Safety-net edit (developer-approved at the red-flag stop):** `the_commit_phase_writes_boot_backup_and_fsinfo_before_clearing_the_checkpoint` computed its indices in a window opening at the primary boot-sector write, which encoded "primary first" — the exact ordering this fix inverts. The window now spans the whole journal; all three assertions (`backup < clear`, `fsinfo < clear`, journal ends with `Sync`) are byte-identical and still hold. The incidental "primary first" pin is gone because it contradicts the fix; the new test above pins the inverted order explicitly.
- **Update 2026-08-01 (d78205f), one blocker cleared, status unchanged:** the correction above ends "Closing F-R26 properly requires [F-R4]". [F-R4] is now **Fixed** — `ResizeOptions::ignore_backup_mismatch` exists and the CLI's `--ignore-backup-mismatch` reaches it — so a filesystem caught in the `BackupMismatch` window is no longer a dead end for a user willing to vouch for the primary. That clause of the correction ("`--force` is still not plumbed into the library") no longer holds; the rest does. This is **not** enough to close F-R26, for two reasons stated in the correction itself and unaffected by the [F-R4] fix: recovery still requires a human to assert which copy is correct, rather than the tool making a valid primary **authoritative** over a differing backup; and the finding's first half — no checkpoint is written at all on the no-FAT-growth path — is untouched, pending the [F-R56] reachability question. The QEMU coverage caveat above also still stands.
- **Status date:** 2026-07-31 00:00 UTC
- **Status commit:** 817831d

### [F-R27] Mount guard is TOCTOU-race-prone, opens without `O_EXCL`, and misses loop-mounted images
- **Category:** 30 (security as an afterthought)
- **Impact:** Medium
- **Explanation:** Nothing prevents the target being mounted between the check and the writes, and an image file currently attached to a loop device is never recognised as mounted.
- **Evidence:** `src/system.rs:27-29` (`check_not_mounted`), excerpt: `if let Ok(resolved_mount) = resolve_device_path(Path::new(mount_device)) { if resolved_mount == device_path {` — canonicalized string comparison only, so a mounted image appears in `/proc/mounts` as `/dev/loop0` and never matches the path the user passed. `src/device.rs:31-34` `OpenOptions::new().read(true).write(writable).open(&path_buf)` — no `custom_flags(libc::O_EXCL)`, unlike `resize2fs`, which has the kernel enforce exclusivity. The check at `executor.rs:325` precedes the open at `:329-333`.
- **Found by:** Security & Code Quality, Integration & Data
- **Caveats:** [S-P].

### [F-R28] No I/O is bounds-checked against `total_sectors`
- **Category:** 30 (security as an afterthought)
- **Impact:** Medium
- **Explanation:** On an image file — the mode every integration and QEMU test exercises — a write past EOF silently extends the file instead of erroring, so a bad sector computation grows the image rather than being caught.
- **Evidence:** `src/device.rs:122-126` (`write_sectors`), excerpt: `let offset = start_sector * self.sector_size as u64; self.file.write_all_at(data, offset)?;` — `self.total_sectors` is never referenced; same in `read_sectors` (`:107-114`). The only guard is the buffer-length check at `:130-139`.
- **Found by:** Security & Code Quality
- **Note:** This is what lets [F-R2]'s misaddressed writes go undetected.
- **Status:** Fixed
- **Status reason:** `Device::check_bounds` now gates both `read_sectors` and `write_sectors` against `total_sectors`, raising the new `Error::SectorOutOfBounds { path, sector, count, total_sectors }`, which the CLI maps to `exit::INTERNAL` -- an out-of-range sector means the tool's own arithmetic was wrong, not that the device misbehaved. The addition is `checked_add`, so a start sector derived from on-disk values cannot wrap into range. `write_sectors` rounds its buffer length up, so a multi-sector run straddling the end is rejected whole rather than by its first sector. Covered by `writing_past_the_end_of_the_device_is_rejected_and_does_not_extend_it`, `a_multi_sector_write_straddling_the_end_is_rejected` (which also asserts a run ending exactly at the last sector is still allowed) and `a_wrapping_sector_range_is_rejected`. The full QEMU suite passing confirms the guard rejects nothing a real resize legitimately does.
- **Interaction with [F-R30]:** the guard intercepted the read that [F-R30]'s safety-net test used to provoke an I/O failure, so that test would have stopped exercising [F-R30]'s guarantee (path + sector + errno on a real I/O error) while still appearing to pass on a different error. Its fixture was changed -- open at 8 sectors, then shorten the file behind the device's back -- so sector 4 is in bounds but backed by nothing, and the failure is again a genuine `DeviceIo`. All three of its assertions are byte-identical to before.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 8ced202

### [F-R31] All failure modes collapse to exit code 1
- **Category:** 24 (inconsistent API contracts)
- **Impact:** Medium
- **Explanation:** Every error path goes through `anyhow`/`bail!`, so "already at maximum size", "device mounted", "checkpoint corrupted" and "not FAT32" are indistinguishable to a calling script.
- **Evidence:** `src/main.rs:122` `fn main() -> Result<()>` with `anyhow::Result`; failures at `:147, :163, :174, :191, :233` all via `bail!`/`?`. No `std::process::exit` with a mapped code anywhere in the crate, despite `error.rs` defining 19 variants.
- **Found by:** Integration & Data
- **Note (2026-07-28, fix session):** Validated as still present; safety-net tests written and committed in `5fe2168` (distinct_failure_modes_stay_non_zero_and_distinguishable, failures_name_the_device_they_refer_to). Not fixed — deferred with the rest of the error-contract batch.
- **Status:** Fixed
- **Status reason:** `main` splits into `run() -> anyhow::Result<()>` and a thin `fn main() -> ExitCode` that maps failures through `exit_code_for`. Codes are **grouped by what the caller should do** (device unopenable 3, mounted 4, not usable FAT32 5, too small 7, backup mismatch 8, needs recovery 9, I/O 10, root required 11, internal 1) rather than one per variant: the exit code is an ABI and this enum changes shape too often to be one — this session alone added four variants and removed six. 2 is left free for clap's usage errors and everything stays below 125. The match on `Error` is exhaustive, so adding a variant now *forces* an exit-code decision at compile time rather than silently landing in a catch-all. The three `anyhow::bail!` sites became typed errors, which was required rather than cosmetic: "already at maximum size" is one of the four cases this finding names, and a bare `bail!` cannot carry a code. Two reused existing variants (`AlreadyMaxSize`, `BackupMismatch`); the privilege check needed a new `Error::RootRequired`. Error rendering is unchanged — `eprintln!("Error: {:?}", err)` reproduces exactly what `anyhow`'s own `fn main() -> Result` printed, so every existing message assertion still holds. The exit-code table is now documented in `README.md`, since an undocumented scheme is only half a contract. Verified: 79 unit + 11 error-contract + 5 fsinfo + 12 integration pass, `cargo fmt --check` and `clippy -D warnings` clean. No safety-net test was edited — `distinct_failure_modes_stay_non_zero_and_distinguishable` passes unchanged.
- **Behaviour change (developer-approved):** "already at maximum size" now exits **0**, not non-zero, making `resize` idempotent and safe to call unconditionally from a provisioning script that does not track whether it has already run. Covered by its own red/green tests (`a_filesystem_already_at_maximum_size_exits_zero`, `resizing_an_already_maximal_filesystem_twice_succeeds_twice`), which also assert the image is byte-identical afterwards and that the reason is still printed. Code 6 remains allocated but unreachable, so the mapping stays complete if that decision is ever revisited.
- **External test updated (developer-approved):** `scripts/test-resize.sh::test_already_max_size` asserted the old "should fail" contract and broke — the shell suite, which CI runs at `.github/workflows/tests.yml:85`, aborted at test 5. It now asserts exit 0 plus a "maximum size" explanation, and additionally re-runs the resize to pin idempotency. This was a genuine break caused by the fix, not a pre-existing failure: **the suite was missing from this session's pre-flight baseline**, which recorded only `cargo test` and the `--ignored` integration tests. It is runnable locally and should be in the baseline of any future session.
- **Latent defect found and fixed while investigating:** `scripts/test-resize.sh` built the binary only `if [ ! -f "$BINARY" ]`, so a stale binary from an earlier commit was tested silently — the first run of this investigation reported "All tests passed" against a binary built nine hours earlier, and was caught only because the error text looked wrong. The script now always rebuilds, resolves the path through `CARGO_TARGET_DIR` when set (this repo redirects it, so `$PROJECT_DIR/target/release` was a leftover from a previous layout rather than the fresh build), fails loudly if no binary results, and logs which binary it is testing. A harness that can silently validate the wrong binary defeats every test in it.
- **Note for [F-R41]:** the no-op case is currently detected twice — once by the CLI's `can_grow()` pre-check in `run()`, once by `AlreadyMaxSize` surfacing from the library — and both must return 0 for the behaviour to be consistent. That duplication *is* [F-R41]; when the CLI stops recomputing the library's decision, the `run()` branch should collapse into the mapping.
- **Status date:** 2026-07-28 19:45 UTC
- **Status commit:** 32c0894

### [F-R33] Six error variants are never constructed
- **Category:** 29 ("utility" dumping ground) / 31 (dead code)
- **Impact:** Medium
- **Explanation:** The taxonomy advertises safety checks — cluster-in-use detection, FAT corruption detection, post-relocation verification — that the code does not perform.
- **Evidence:** `src/error.rs:24-57` — verified by grep rather than by trusting a count: zero construction sites for `DeviceTooSmall` (`:27`), `Relocation` (`:42`), `Verification` (`:45`), `ClusterInUse` (`:48`), `NoFreeCluster` (`:51`), `CorruptedFAT` (`:57`). Excerpt: `#[error("Cluster {0} is in use and would be overwritten by FAT growth")] ClusterInUse(u32)`.
- **Found by:** Error Handling & Observability, Security & Code Quality, Structure & Boundaries (3 specialists)
- **Note (2026-07-28):** Still open, but now five variants rather than six — the [F-R1] fix constructs `Relocation` for the two cases where an interrupted data shift cannot be resumed safely. `DeviceTooSmall`, `Verification`, `ClusterInUse`, `NoFreeCluster` and `CorruptedFAT` remain unconstructed. Note that `Verification` was the variant a real `verify_relocation` would have used; [F-R20] deleted that stub rather than implementing it, so this variant is now dead by decision rather than by oversight.
- **Note (2026-07-28, fix session):** Validated: five variants remain unconstructed (`DeviceTooSmall`, `Verification`, `ClusterInUse`, `NoFreeCluster`, `CorruptedFAT`). No flaw-specific safety net was written, deliberately — if this resolves by *deleting* them the compiler is the safety net, but if it resolves by *constructing* any of them that is new behaviour needing its own red/green tests. That construct-vs-delete choice is per variant and unmade.
- **Status:** Fixed
- **Status reason:** Resolved per variant, not in bulk — four deleted, one constructed. Deleted: `Verification` (F-R20 deliberately removed the no-op verifier rather than implementing it, so this was dead *by decision*), `ClusterInUse` and `NoFreeCluster` (residue of the abandoned cluster-renumbering design; the data-shift approach never needs either, and `find_free_cluster`, the only would-be constructor, is itself dead per [F-R34]), and `CorruptedFAT` (nothing detects FAT corruption, and constructing it would mean *building* detection — a feature, not this fix). Constructed: `DeviceTooSmall`, which turned out to invert the finding's framing. `calculate_new_size` has exactly one production caller (`executor.rs:126`) and it passes the device's *actual* size; the tool offers no way to request a smaller one. So `new_total_sectors < old_total_sectors` never means "the user asked to shrink" — it means a truncated image, the wrong device, or a corrupt `total_sectors` field. That case was reporting `ShrinkNotSupported` ("Cannot shrink filesystem (not supported)"), describing an operation the user did not request and the tool does not offer. `DeviceTooSmall { current, minimum }` now carries it and names both numbers; `ShrinkNotSupported` was deleted as the genuinely dead variant. Deleting `DeviceTooSmall` instead would have cemented the wrong message. Audited afterwards: **every** variant in `Error` now has at least one construction site outside `error.rs`. This is a public API change (five variants removed). Verified: 79 unit + 8 error-contract + 5 fsinfo + 12 integration pass, `cargo fmt --check` and `clippy -D warnings` clean.
- **Note on reachability:** `DeviceTooSmall` is reachable through the library entry point (`resize_fat32` → `calculate_new_size`), but **not currently through the CLI**: `main.rs` intercepts a too-small device with its own `can_grow()` pre-check and reports `AlreadyMaxSize` first. That shadowing is [F-R41] (the CLI recomputing the decision the library already owns); fixing F-R41 will surface this message to CLI users. Recorded rather than worked around, since widening the fix into `main.rs` would have pre-empted F-R41's own decision.
- **Status date:** 2026-07-28 19:15 UTC
- **Status commit:** bb3c6c3

### [F-R34] Seven public FAT-manipulation functions are never called
- **Category:** 29 ("utility" dumping ground) / 31 (dead code)
- **Impact:** Medium
- **Explanation:** Leftovers from the pre-refactor cluster-renumbering design that the current data-shift approach no longer needs, kept alive and shielded from dead-code warnings only by being `pub` in a library.
- **Evidence:** `src/fat32/operations.rs:133-297` — verified by grep across `src/` and `tests/`: the sole reference to each of `write_fat_entries`, `read_fat_entry`, `write_fat_entry`, `write_fat_entry_with_size`, `read_cluster`, `write_cluster`, `find_free_cluster` outside `operations.rs` is the `pub use` block at `fat32/mod.rs:9-14`. `count_free_clusters` is called only from its own unit test (`:317`). `find_free_cluster` carries a `_device: &Device` parameter it ignores (`:271`).
- **Found by:** Security & Code Quality, Structure & Boundaries, Coupling & Dependencies (3 specialists)
- **Status:** Fixed
- **Status reason:** Seven of the eight are deleted — `write_fat_entries`, `read_fat_entry`, `write_fat_entry`, `write_fat_entry_with_size`, `read_cluster`, `write_cluster` and `find_free_cluster` — together with their entries in the `fat32/mod.rs` re-export list, taking `operations.rs` from 358 to 170 lines. With them out of that list the dead-code lint sees `operations.rs` for the first time, which is the durable half of the fix: a future dead helper now fails CI instead of hiding behind `pub`. **`count_free_clusters` was deliberately kept** at the developer's direction. It is equally uncalled today, but it is the function [F-R45] names as the way to reconcile the FSInfo free count against the FAT, so deleting it would have foreclosed that fix; unlike its neighbours it is also a coherent operation on a FAT a consumer can already obtain via `read_fat_table`, rather than a fragment of the abandoned cluster-renumbering design. That decision is recorded in a doc comment on the function, including the honest cost: staying re-exported keeps it exempt from the very lint this fix restored, so it will not announce itself if [F-R45] is never done.
- **Test deleted with the code:** `test_find_free_cluster` went with `find_free_cluster`, and it is worth recording *why* it was safe to delete: it never called the function. It reimplemented the search inline with iterators and asserted on that, so it would have kept passing after the deletion while covering nothing — a test whose name claimed coverage the suite did not have. A note in its place records this. The suite therefore goes 129 → 128 by exactly this one test, with no other case lost.
- **Interaction with [F-R30]:** deleting `write_cluster` removes one of the two synthetic `io::Error` construction sites that [F-R30]'s residual note flagged (`operations.rs`, alongside `system.rs:13`) — the conflation of an argument error with an I/O error. One site remains.
- **Status date:** 2026-07-28 21:20 UTC
- **Status commit:** 9194c71

### [F-R35] `ClusterId`/`SectorNum` newtypes are entirely unused
- **Category:** 13 (inconsistent boundaries) / 29
- **Impact:** Medium
- **Explanation:** ~117 lines of type-safety machinery — constructors, `new_checked`, `index()`, `Display`, `From` conversions — exist and are re-exported, but nothing constructs or consumes either type.
- **Evidence:** `src/fat32/structs.rs:3-119`, doc comment: `/// This newtype prevents accidentally mixing up cluster IDs with sector numbers.` The only reference outside `structs.rs` is `pub use structs::{fat_entry, BootSector, ClusterId, FSInfo, SectorNum};` at `fat32/mod.rs:6`. `git log --oneline src/fat32/structs.rs` → added by the most recent commit, 238b305 "bit code cleanup". Meanwhile `relocator.rs:10-19` still declares `ClusterMove { from_cluster: u32, from_sector: u64, to_sector: u64 }` — exactly the cluster/sector confusion the newtypes were meant to prevent, and exactly where [F-R2] lives.
- **Found by:** Structure & Boundaries, Coupling & Dependencies, Security & Code Quality (3 specialists)
- **Status:** Fixed
- **Status reason:** Both newtypes are deleted (118 lines from `structs.rs`, plus the `fat32/mod.rs` re-export). Delete was chosen over adopt after establishing that **the newtypes as written were not type-*safe*, only type-*distinct***: `SectorNum::offset` was `self.0 + offset` and `to_byte_offset` an unchecked multiply, so threading them through the relocator would have reintroduced unchecked arithmetic into precisely the paths [F-R7] and [F-R28] hardened earlier in this session; and `ClusterId::index()`'s `saturating_sub(2)` maps clusters 0 and 1 silently to index 0, which *hides* the defect [F-R36] is about rather than catching it. Adopting them was therefore not the cheap route to type safety it appears to be — it would have been a rewrite wearing the old code's name, and one that made the next flaw in this batch harder to fix correctly. The reasoning is recorded at the re-export site so a future reader does not re-propose the newtypes without also fixing them.
- **On the finding's strongest argument:** the evidence points at `ClusterMove` as "exactly the cluster/sector confusion the newtypes were meant to prevent, and exactly where [F-R2] lives". That argument was addressed structurally instead of typographically: [F-R44]'s fix deleted `ClusterMove::to_cluster`, so the struct now carries one cluster number and two sector numbers rather than two of each, and the class of confusion the newtypes targeted has one fewer place to occur.
- **Coverage:** none, by agreement — the deleted items had zero constructors and zero consumers crate-wide, so no execution path ran through them and the compiler is the safety net. Confirmed by the suite being byte-for-byte unchanged at 128 passing before and after.
- **Status date:** 2026-07-28 21:20 UTC
- **Status commit:** bc81d9d

### [F-R36] `cluster_to_sector` does unchecked `cluster - 2` while the safe helper beside it is unused
- **Category:** 28 (magic numbers) / 20
- **Impact:** Medium
- **Explanation:** Panic in debug, ~4-billion-sector offset in release, for cluster 0 or 1.
- **Evidence:** `src/fat32/structs.rs:408`, excerpt: `self.first_data_sector() + ((cluster - 2) as u64 * self.sectors_per_cluster() as u64)`, against the unused `:47-49` `pub const fn index(self) -> u32 { self.0.saturating_sub(2) }`. Same bare pattern at `relocator.rs:90-91`.
- **Found by:** Error Handling & Observability
- **Status:** Fixed
- **Status reason:** `cluster_to_sector` returns `Option<u64>` and is fully checked — `checked_sub(2)`, `checked_mul` by `sectors_per_cluster`, `checked_add` to `first_data_sector` — matching `data_sectors()` and `data_clusters()` directly above it, which already returned `Option` for the same reason: every input comes off the disk and is attacker-controlled. `plan_relocation` now *calls* it instead of recomputing `(cluster - 2) * sectors_per_cluster` inline, mapping `None` to `Error::Relocation`. So the bounds check exists once rather than needing to be repeated at each copy of the formula. Covered by `the_reserved_clusters_have_no_sector` (clusters 0 and 1 → `None`, cluster 2 → the data-area start, since 2 is the boundary and is valid) and `a_cluster_whose_sector_would_overflow_has_no_sector`.
- **Note on the fix's shape, which the finding could not anticipate:** by the time this was reached, [F-R34] had deleted `read_cluster` and `write_cluster` — the only two production callers `cluster_to_sector` had. It was therefore public API with **zero** internal callers, while the live production instance of the defect was the hand-inlined copy in `plan_relocation`. That is the same duplication [F-R13] attacked, and it was *why* the accessor had no caller. Having `plan_relocation` adopt the accessor fixes both at once. The equivalence of the two formulas was not assumed: the pre-existing `plan_relocation_maps_clusters_to_boot_derived_sectors` asserts the plan agrees with `cluster_to_sector`, so it was verified by test before the change.
- **On not saturating:** returning `None` was chosen over clamping deliberately. `ClusterId::index`, deleted by [F-R35] in this same batch, used `saturating_sub(2)`, which maps clusters 0 and 1 to the *first data cluster's* sector — a plausible, wrong answer that reads as success. The report's own framing ("the safe helper beside it is unused") slightly overstates that helper: adopting it would have replaced a loud failure with a quiet one.
- **Safety-net integrity:** the signature change touched 26 call sites across `structs.rs` and `relocator.rs`. Every edit was `Some(...)` wrapping or `.unwrap()` — call shape only. Verified by diff: every expected *value* is byte-identical, and no assertion was relaxed, skipped or deleted. `plan_relocation_maps_clusters_to_independently_computed_sectors`, written in the safety-net phase with hand-derived sector literals precisely so it would not move with the code, passed **completely unedited** across the rewrite.
- **Status date:** 2026-07-28 21:20 UTC
- **Status commit:** f2cce0b

### [F-R37] Executor reaches past `fat32::operations` to write FAT areas as raw sectors
- **Category:** 13 (inconsistent boundaries)
- **Impact:** Medium
- **Explanation:** FAT table I/O is `operations`' job, but the executor computes FAT-copy sector arithmetic itself and drives `device.read_sector`/`write_sector` directly, putting on-disk FAT layout knowledge in two layers.
- **Evidence:** `src/resize/executor.rs:687-700` (`sync_fat_copies`), excerpt: `let fat_dest_start = fat1_start + (fat_num as u64 * calc.new_fat_size as u64); … let data = device.read_sector(src_sector)?; device.write_sector(dest_sector, &data)?;`; same pattern in `init_new_fat_sectors` (`:666-675`). Compare the equivalent arithmetic owned by `src/fat32/operations.rs:216`.
- **Found by:** Structure & Boundaries

### [F-R38] plan/execute/verify signatures demand dependencies they ignore
- **Category:** 23 (dependency injection misuse)
- **Impact:** Medium
- **Explanation:** The relocator's three entry points take a `&Device`, a mutable FAT slice, `new_fat_size` and `new_data_clusters` that are all discarded, so the signatures advertise data dependencies the implementations do not have.
- **Evidence:** `src/resize/relocator.rs:52-58, 125-133, 178-185` — `plan_relocation(_device: &Device, …, _new_data_clusters: u32)`; `execute_relocation(device, boot, _fat: &mut [u32], plan, _new_fat_size: u32, _new_data_clusters: u32, verbose)`; `:168-170` `// No cluster number changes, so return empty vector … Ok(Vec::new())`. The executor still constructs and threads these values through at `:455-462` and `:493-501`.
- **Found by:** Coupling & Dependencies, Structure & Boundaries
- **Status:** Fixed
- **Status reason:** Resolved for two of the three entry points as a side effect of the [F-R2] fix: `plan_relocation` no longer takes `_device` or `_new_data_clusters`, and `execute_relocation` no longer takes `_fat`, `_new_fat_size` or `_new_data_clusters` and returns `()` instead of an always-empty `Vec`. The remaining entry point, `verify_relocation(_fat, _first_affected, _last_affected)`, was deleted outright with [F-R20] — it was a do-nothing stub, so the three ignored parameters went with it. No relocator entry point now declares a dependency it does not use.
- **Status date:** 2026-07-28 09:55 UTC (completed; partial fix 2026-07-28 00:00 UTC)
- **Status commit:** 960a851, completed in 614ba58

### [F-R39] `read_boot_sector` and `read_boot_sector_for_recovery` are ~20-line near-verbatim copies
- **Category:** 9 (shotgun surgery)
- **Impact:** Medium
- **Explanation:** Any change to the sector-size bootstrap must be made twice or the recovery path diverges from the normal path.
- **Evidence:** `src/fat32/operations.rs:21-36` and `:47-62` are byte-identical modulo comments, diverging only at `:37` `validate_boot_sector(&boot)?;` vs `:63` `validate_boot_sector_for_recovery(&boot)?;`. `validation.rs:5-17` already solves this exact split correctly via one parameterized `validate_boot_sector_impl(boot, allow_invalidated)` — the right pattern exists in the codebase.
- **Found by:** Structure & Boundaries
- **Status:** Fixed
- **Status reason:** Resolved as a side effect of the [F-R23]/[F-R40] fix, which had to touch both readers anyway. The shared bootstrap now lives once in `read_boot_sector_unvalidated`; `read_boot_sector` and `read_boot_sector_for_recovery` are four lines each and differ only in which validator they call -- the same parameterized-shared-impl shape this finding's evidence pointed at in `validate_boot_sector_impl`. The sector-size bootstrap itself moved further out still, into `bootstrap_sector_size`, so it now exists in exactly one place for both readers and the device opener. Not separately tested: the existing readers' tests plus `read_boot_sector_returns_the_same_answer_on_repeated_calls` cover both paths through the shared helper.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 7d278d9

### [F-R40] `read_boot_sector*` mutates device geometry as a side effect
- **Category:** 12 (hidden side effects)
- **Impact:** Medium
- **Explanation:** Functions named "read…" take `&mut Device` and reconfigure the device's sector size and total-sector count, so the meaning of every later `read_sector`/`write_sector` depends on a state change performed by a reader.
- **Evidence:** `src/fat32/operations.rs:33, :59` `device.set_sector_size(sector_size as u32);` inside `pub fn read_boot_sector(device: &mut Device)`. Documented at `:16-18`, but it is the mechanism behind [F-R23].
- **Found by:** Error Handling & Observability, Structure & Boundaries
- **Status:** Fixed
- **Status reason:** Resolved together with [F-R23], which removed the setter these readers called. `read_boot_sector` and `read_boot_sector_for_recovery` now take `&Device` and are pure reads -- the geometry they used to install is established at construction by `open_fat32_device`. A reader can no longer change what a later `read_sector`/`write_sector` means. Covered by `read_boot_sector_returns_the_same_answer_on_repeated_calls` (which the `&Device` signature makes expressible) and `device_geometry_matches_a_4096_byte_sector_filesystem`.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 7d278d9

### [F-R41] CLI recomputes the resize decision and target size the library already owns
- **Category:** 25 (business logic in the UI)
- **Impact:** Medium
- **Explanation:** `main.rs` independently decides growability, picks the new size and computes the increase before calling `resize_fat32`, which then derives its own numbers — so the user sees the CLI's prediction, not the library's plan, and the two can drift.
- **Evidence:** `src/main.rs:162` `if !info.can_grow { anyhow::bail!("Filesystem is already at maximum size…"` duplicating `Error::AlreadyMaxSize` (`calculator.rs:70-72`); `:177` `let new_size = info.max_new_size_bytes.unwrap_or(info.current_size_bytes);`; `:199` `let increase = new_size.saturating_sub(current_size);` — all printed before `resize_fat32(options)` at `:232`.
- **Found by:** Error Handling & Observability, Structure & Boundaries

### [F-R42] `executor.rs` bundles five unrelated concerns
- **Category:** 11 (low cohesion)
- **Impact:** Medium
- **Explanation:** Fault injection, an on-disk record format, options/result DTOs, the resize algorithm, and a read-only query with ~70 lines of terminal formatting all share one file.
- **Evidence:** `src/resize/executor.rs` — `maybe_crash_at` (`:29-44`), `ResizeCheckpoint` + codec + sector I/O (`:46-255`), `ResizeOptions` builder (`:257-301`), `resize_fat32` (`:321-627`), `get_fs_info`/`FSInfoReport` (`:707-771`) plus `impl std::fmt::Display for FSInfoReport` (`:773-841`), excerpt: `writeln!(f, "FAT32 Filesystem Information")?; writeln!(f, "============================")?;` — human-facing rendering that belongs on the CLI side of the boundary.
- **Found by:** Structure & Boundaries
- **Status:** Fixed
- **Status reason:** All five concerns now have their own home, and `executor.rs` went from 1422 to 491 lines holding only the resize algorithm and its two private FAT helpers. Fault injection → `resize/fault.rs` (crate-private; nothing outside the resize path may trigger a simulated crash). Checkpoint record, codec and sector I/O → `resize/checkpoint.rs`. `ResizeOptions`/`ResizeResult` → `resize/options.rs`. `get_fs_info`/`FSInfoReport` → `resize/fsinfo.rs`. The ~70-line `Display` impl moved off the library entirely: it is now `render_fs_info` in `main.rs`, so `FSInfoReport` is data and the terminal formatting sits on the CLI side of the boundary the report identified. This is a public API change (`FSInfoReport` no longer implements `Display`), taken with the developer's explicit approval; the upside is that a library consumer gets fields instead of prose to parse. Layering stays strictly acyclic — `checkpoint` and `options` do not import `executor`. The checkpoint *functions* deliberately stayed crate-private rather than being re-exported alongside the types, with a comment in `resize/mod.rs` recording why; exporting a record an external caller could build but never apply is the incoherence [F-R44] flags, and widening it here would have made that flaw worse. Verified name-for-name: all 20 `#[test]` functions from the old `executor.rs` survive the split (three lost their `#[test]` attribute at an extraction boundary mid-refactor and were restored — the suite count catching that is why the pre-flight baseline is recorded per-file). Full suite green at every gate: 73 unit + 5 error-contract + 5 fsinfo + 12 integration, clippy `-D warnings` clean, `cargo fmt --check` clean, and the `fault-injection` feature still builds so the crash-recovery harness is unaffected.
- **Status date:** 2026-07-28 17:05 UTC
- **Status commit:** b0e3a17
- **Note:** `main.rs` absorbed ~85 lines taking on the renderer, which adds to the low-cohesion pressure [F-R54] already notes there. The rendering belongs CLI-side, but `main.rs` is now itself a split candidate.

### [F-R44] `pub mod` + `pub use` yields three public paths per type, exports unusable types, and suppresses the dead-code lint
- **Category:** 6 (leaky abstractions) / 31
- **Impact:** Medium
- **Explanation:** Every module is both declared `pub` and re-exported, creating multiple public paths with no stated stability difference — and making every internal helper look reachable to the compiler.
- **Evidence:** `BootSector` is reachable as `fat32expander::BootSector` (`lib.rs:9`), `::fat32::BootSector` (`fat32/mod.rs:6`) and `::fat32::structs::BootSector` (`fat32/mod.rs:2`). `resize/mod.rs:9-12` exports `ResizeCheckpoint` and `ResizePhase`, but `write_checkpoint`/`read_checkpoint`/`clear_checkpoint`/`check_for_incomplete_resize` (`executor.rs:204-255`) are private — an external user can build and serialise a checkpoint but never apply one. `.github/workflows/tests.yml:41` runs `cargo clippy -- -D warnings` and passes despite ~15 unused public items, because `pub` items in a library are exempt from dead-code analysis.
- **Found by:** Coupling & Dependencies, Security & Code Quality
- **Note:** This is why [F-R33]/[F-R34]/[F-R35]/[F-R48] are invisible to the toolchain.
- **Status:** Fixed
- **Status reason:** The crate now exposes items on exactly two levels with a **stated** difference between them, documented in `lib.rs`'s module docs: the crate root is the supported API, and `fat32` is the lower-level namespace for callers who want to read a boot sector or FAT table without driving a resize. Everything else is private — `device`, `error`, `resize` and `system` at the root, and the `structs`/`operations`/`validation` leaves under `fat32` — so `::fat32::structs::BootSector` and the whole `::resize::` path are gone and each item has one path within its level. `ResizeCheckpoint`/`ResizePhase` are no longer exported at all: every function that reads, writes or applies a checkpoint is crate-private, so the export let an external caller build and serialise a crash-recovery record it had no way to act on. `SizeCalculation` moved the *other* way and is now exported from the root, because it is the type of `ResizeResult::calculation`, a public field — a public field whose type a caller cannot name is the same incoherence pointing backwards. Covered by `tests/public_api_test.rs`, whose 8 cases passed **unedited** across this fix, which is the evidence that the narrowing removed nothing load-bearing.
- **Correction to the evidence:** the finding attributes the dead-code blindness to `pub mod`. That is not the operative `pub`. Making every leaf module private was verified to produce **zero** new warnings, because the curated `pub use` block still re-exported the dead items; narrowing *that list* is what surfaces them. So F-R44 does not itself un-blind the lint — it establishes the shape in which [F-R34] and [F-R35]'s removals become visible. The "~15 unused public items" figure is also not reproducible as stated: with the `resize` half narrowed, the compiler reported exactly 2 (see below), and a probe narrowing the `fat32` re-export list reported the 7 [F-R34] names.
- **Dead code exposed by this fix (developer-approved, handled here):** un-blinding the lint immediately failed `clippy -D warnings`, which CI runs, on two items neither this flaw nor [F-R34] had listed — both dead in production but alive in tests, a category the report does not otherwise name. `ResizeCheckpoint::with_shift_progress` is a builder used only by four checkpoint tests (the resize path assigns `shift_progress` directly), and is now `#[cfg(test)]`, which states what it is instead of leaving it looking like production API. `ClusterMove::to_cluster` was written at one site and never read; it was always equal to `from_cluster` by construction, so deleting it turns "relocation never renumbers clusters" from a field two places had to keep in agreement into a property the type cannot express otherwise. `#[allow(dead_code)]` was rejected for both: it would reinstate exactly the blindness this flaw is about.
- **Safety-net assertion removed (developer-approved):** deleting `to_cluster` removed `assert_eq!(mv.to_cluster, cluster, "cluster numbers must not change")` from `plan_relocation_maps_clusters_to_independently_computed_sectors`, committed as safety net in `967645c`. This is the one edit of the session that goes beyond mechanical adaptation, so it was put to the developer explicitly rather than applied as a call-shape change. The assertion's *guarantee* survives structurally — a `ClusterMove` no longer has two cluster fields that could disagree — and the test's other three assertions per move are unchanged.
- **Status date:** 2026-07-28 21:20 UTC
- **Status commit:** 2dae2ab

### [F-R58] Hand-chunked FAT read with a stale size comment, materialising the whole FAT twice
- **Category:** 8 (premature optimization)
- **Impact:** Medium
- **Explanation:** Chunking was added without measurement — the labelled size is wrong for three of the four supported sector sizes, the data is copied twice, and the genuinely hot path is left unbatched.
- **Evidence:** `src/fat32/operations.rs:106` `const CHUNK_SECTORS: u32 = 256; // Read 128KB at a time` (true only at 512-byte sectors; 1 MB at 4096). Both `fat_data` (`:103`) and `entries` (`:117`) hold the full FAT. The resulting vector is used only for `fat_entry::is_free(entry)` and `fat.len()` in `plan_relocation` (`relocator.rs:79-86`). Meanwhile `sync_fat_copies` does unbatched single-sector read/write pairs (`executor.rs:695-701`).
- **Found by:** Coupling & Dependencies

### [F-R60] CLAUDE.md documents an algorithm the code deliberately abandoned
- **Category:** 34 (inconsistent conventions) + steering-file contradiction
- **Impact:** Medium
- **Explanation:** The project guide describes the pre-refactor cluster-renumbering design; the implementation preserves cluster numbers and shifts only physical sectors. TECHGUIDE.md agrees with the code, so the two steering files disagree with each other.
- **Evidence:** CLAUDE.md "Directory Entry Updates": *"When clusters are relocated, ALL directory entries pointing to moved clusters must be updated."*; "Common Pitfalls" §3: *"If cluster 2 is relocated, the boot sector's root cluster field must be updated."* Against `src/resize/relocator.rs:122` `/// Since cluster numbers don't change, no FAT chain or directory entry updates are needed!` and `:169` `// The root cluster stays at cluster 2 (just at a different physical location)`. `BootSector::set_root_cluster` (`structs.rs:276`) has zero call sites. `TECHGUIDE.md:162` matches the code.
- **Found by:** Structure & Boundaries, Coupling & Dependencies
- **Note:** CLAUDE.md's "Critical insight" paragraph (old vs. new `first_data_sector`) *does* match the code — only the two sections above contradict it.
- **Status:** Fixed
- **Status reason:** Both stale sections rewritten to the design the code implements. "Directory Entry Updates" became "Cluster Numbers Are Preserved — No Directory Entry Updates", stating that relocation moves physical sectors only and that FAT chains, directory entries and `root_cluster` therefore need no rewriting, with a historical note explaining that the renumbering design was abandoned and that `set_root_cluster`'s zero call sites are residue from it. Pitfall §3 was inverted to say that rewriting `root_cluster` during a resize is a bug, and now points at the safety-net test that guards it. The "Critical insight" paragraph was already correct and is untouched. CLAUDE.md and TECHGUIDE.md now agree. Documentation only; no code changed.
- **Status date:** 2026-08-02 08:45 UTC
- **Status commit:** _(recorded at the end of the session)_

### [F-R32] `--dry-run` skips crash detection, so the preview is not the plan that will run
- **Category:** 12 (hidden side effects) / 24
- **Impact:** Low
- **Explanation:** On a filesystem with a pending checkpoint, the dry run computes a fresh plan while the real run takes the resume path with different FAT sizes — the preview is non-representative exactly where preview matters most.
- **Evidence:** `src/resize/executor.rs:348-353`, excerpt: `let incomplete_resize = if !options.is_dry_run() { check_for_incomplete_resize(&device, &boot)? } else { None };` — dry run falls through to `calculate_new_size` (`:420`), the real run branches at `:390-418`.
- **Found by:** Error Handling & Observability, Integration & Data

### [F-R24] `set_sector_size` silently swallows a stat failure, leaving `total_sectors` stale
- **Category:** 12 (hidden side effects) / 20
- **Impact:** Low
- **Evidence:** `src/device.rs:82-88`, excerpt: `if let Ok(file_size) = self.file_size() { self.total_sectors = file_size / size as u64; }` — on failure `total_sectors` keeps the value computed with 512, and `write_checkpoint` (`executor.rs:205`) then targets the wrong sector on a 4096-byte-sector device.
- **Found by:** Error Handling & Observability
- **Status:** Fixed
- **Status reason:** Resolved by [F-R23]'s removal of `set_sector_size`. There is no longer a setter that can leave `total_sectors` stale: the size is read once in `Device::open_with`, and `total_sectors` is derived from it in the same expression that constructs the device, with the `?` on `size_of` propagating a stat failure instead of swallowing it. Covered by `total_sectors_always_matches_the_device_size_divided_by_the_sector_size`.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 7d278d9

### [F-R29] All open failures collapse to "not found", discarding errno
- **Category:** 20 (weak error handling strategy)
- **Impact:** Low
- **Evidence:** `src/device.rs:31-35` (`open_impl`), excerpt: `.map_err(|_| Error::DeviceNotFound(path_display))?` — EACCES, EBUSY, EROFS and EIO all print "Device 'X' not found or cannot be opened". Exactly the path a non-root user hits after `--force` (`main.rs:146-149`).
- **Found by:** Error Handling & Observability
- **Note (2026-07-28, fix session):** Validated as still present; safety-net tests written and committed in `5fe2168` (opening_a_missing_path_fails_and_names_the_path, opening_a_directory_fails_and_names_the_path, failures_name_the_device_they_refer_to). Not fixed — deferred with the rest of the error-contract batch.
- **Status:** Fixed
- **Status reason:** `Error::DeviceNotFound(String)` is replaced by `Error::DeviceOpen { path, #[source] source: std::io::Error }`, applied at all three construction sites (`device.rs` `open_impl`, and both the Linux and non-Linux halves of `system.rs::get_block_device_size`). The variant was renamed rather than merely extended because "not found" is *false* for EACCES, EBUSY and EROFS — appending the errno to a message that still says "not found" would have made the error more informative without making it correct. The `io::Error` is kept as a `#[source]` and deliberately **not** interpolated into the `Display` string: callers render the chain, so interpolating would print the errno twice; `to_string()` alone still names the path, which is what the device-level tests require. Keeping `source.kind()` reachable also means [F-R31] can map exit codes per errno class without a variant per errno. Confirmed end-to-end: a missing device now reports `Cannot open device 'X'` / `No such file or directory (os error 2)`, and a read-only image under `--force` reports `Permission denied (os error 13)` where it previously claimed the device was not found. This is a public API change (`DeviceNotFound` is gone). Verified: 76 unit + 8 error-contract + 5 fsinfo + 12 integration pass, `cargo fmt --check` and `clippy -D warnings` clean. No safety-net test needed editing.
- **Status date:** 2026-07-28 18:40 UTC
- **Status commit:** b7d449d

### [F-R30] Sector I/O errors carry neither path nor sector though `Device` holds both
- **Category:** 20 (weak error handling strategy) / 34
- **Impact:** Low
- **Evidence:** `src/error.rs:35-36` `Io(#[from] std::io::Error)` propagates bare, so a short read of the backup boot sector surfaces as "IO error: failed to fill whole buffer" with no sector or device. `Device` stores `path: PathBuf` (`device.rs:10`), used only in the `Debug` impl (`:18`).
- **Found by:** Error Handling & Observability
- **Note (2026-07-28, fix session):** Validated as still present; safety-net tests written and committed in `5fe2168` (reading_past_the_end_of_the_device_fails, writing_a_sector_of_the_wrong_length_is_rejected). Not fixed — deferred with the rest of the error-contract batch.
- **Status:** Fixed
- **Status reason:** `Error::DeviceIo { op, path, offset, sector, len, #[source] io::Error }` now carries the device identity on every failed read and write, filled in by a private `Device::io_err` helper — placed on `Device` deliberately, since the path and sector size live there and nowhere else, which is the finding's actual point (`path` was previously used only by the `Debug` impl). Both `offset` and `sector` are reported: byte-addressed calls such as the 4096-byte boot-sector bootstrap cannot name a sector themselves, but `Device` can always derive one. All four I/O methods are wrapped (`read_sectors`, `write_sectors`, `read_bytes_at`, `write_bytes_at`), plus `sync` via a sibling `Error::DeviceSync` which has no offset to report. `write_sector`'s length guard was additionally split out into `Error::SectorLengthMismatch`: it is an argument error that was being dressed as an `io::Error` with `ErrorKind::InvalidInput`, the same conflation one level down. Confirmed end-to-end: a truncated image now reports `Failed to read 4096 byte(s) at offset 0 (sector 0) of device 'x.img'` / `failed to fill whole buffer`, where it previously reported only the latter. Verified: 78 unit + 8 error-contract + 5 fsinfo + 12 integration pass, `cargo fmt --check` and `clippy -D warnings` clean. No safety-net test needed editing.
- **Residual (deliberately out of scope):** `Error::Io` remains reachable from `metadata()`/`try_clone()` in `device.rs` `open_impl` and `file_size` (`:41, :47, :100, :104`), which still surface without the path. These are device-identity operations rather than sector I/O, so they fall outside what this finding describes, but they are the same shape and would be the natural next step. Separately, `system.rs:13` and `fat32/operations.rs:256` construct *synthetic* `io::Error`s to carry non-I/O conditions — the same conflation `SectorLengthMismatch` was split out to remove, and worth its own finding.
- **Status date:** 2026-07-28 18:55 UTC
- **Status commit:** 2d09bc5

### [F-R43] Surfacing one more filesystem field requires four coordinated edits
- **Category:** 9 (shotgun surgery)
- **Impact:** Low
- **Evidence:** `src/resize/executor.rs:730-748, 752-771, 773-842` — `FSInfoReport` mirrors `BootSector` field-for-field: 17 fields, 14 of them straight `boot.*()` copies, each repeated in the struct definition, the construction block and the `Display` impl. Excerpt: `bytes_per_sector: boot.bytes_per_sector(), sectors_per_cluster: boot.sectors_per_cluster(), reserved_sectors: boot.reserved_sectors(),`.
- **Found by:** Structure & Boundaries
- **Status:** Fixed
- **Status reason:** The mirroring is gone rather than shortened. `FSInfoReport` now holds the `BootSector` itself and exposes it via `boot()`, so geometry is answered by the type that owns it; the 17 public fields become 6 private ones (`device_path`, `boot`, `data_clusters`, `free_clusters`, `backup_matches`, `device_sectors`) behind accessors. What the report keeps is only what the boot sector does not know: where the filesystem was read from, how big the *device* is, and the three answers derived from comparing the two (`can_grow`, `current_size_bytes`, `max_new_size_bytes`), which are now computed on demand instead of stored — so they cannot go stale against the boot sector they describe. Surfacing another geometry field now costs **zero** edits here, which is the point: the alternative fixes (one accessor per field, or a mirroring macro) would have reduced the edit count without making the duplication structurally impossible. `data_clusters` is the one derived value still stored, deliberately: `BootSector::data_clusters()` returns `Option`, `get_fs_info` validates it once at construction, and callers get a plain `u32` rather than an `Option` inviting an `unwrap`. This is a public API change (fields → accessors), made with the developer's explicit approval. Call sites in `main.rs`, `tests/integration_test.rs` and `tests/fsinfo_report_test.rs` were updated mechanically; verified that the safety-net assertions were preserved exactly — same assertion count in both test files (31 and 41) and an identical multiset of expected values, so only the call shape moved. Full suite green: 73 unit + 5 error-contract + 5 fsinfo + 12 integration, clippy `-D warnings` and `cargo fmt --check` clean.
- **Status date:** 2026-07-28 17:35 UTC
- **Status commit:** 06bff0a

### [F-R45] FSInfo free count is extrapolated arithmetically, never reconciled against the FAT
- **Category:** 17 (no clear ownership of data)
- **Impact:** Low
- **Evidence:** `src/resize/executor.rs:599-604`, excerpt: `let new_free = if old_free == FSInfo::UNKNOWN_FREE { FSInfo::UNKNOWN_FREE } else { old_free.saturating_add(additional_clusters) };` — the authoritative free-cluster truth is the FAT, but the new value derives from a hint field that may already be stale, and `count_free_clusters` (`operations.rs:291`) that could reconcile it is dead ([F-R34]). On a phase-`FatWritten` resume the same addition is re-applied.
- **Found by:** Integration & Data
- **Cross-reference:** [F-R13].

### [F-R46] Root check is advisory, bypassable with `--force`, and absent from the library entry point
- **Category:** 30 (security as an afterthought)
- **Impact:** Low
- **Evidence:** `src/main.rs:143-149`, excerpt: `if !check_root() && !dry_run { … if !force { anyhow::bail!("Run as root or use --force to continue anyway"); } }`. `resize_fat32` — the public API, and what the integration tests call — never checks privileges. The kernel still enforces access, so this is a defence-in-depth gap rather than privilege escalation.
- **Found by:** Security & Code Quality
- **Status:** Fixed
- **Status reason:** The gate is **removed** rather than pushed down into the library, which is the opposite of what the finding's framing implies and was the developer's call after the options were laid out. `geteuid() == 0` is a bad proxy for "may I write this device", and it was wrong in three legitimate directions: a user resizing a file image they own, a member of group `disk`, and a caller holding `CAP_SYS_ADMIN` without uid 0 were all refused. The kernel is the authority and the `open` already answers the question — since [F-R29]/[F-R30] that failure carries both the errno and the path, so removing the guess costs no diagnostic. `Error::RootRequired` is deleted (it becomes unconstructed) and exit code 11 is retired without being reused, so a script branching on it finds nothing rather than a new meaning; a caller who genuinely cannot write now gets code 3, which is what the kernel actually reported. The gate's escape hatch is what made it actively harmful: `--force` switched it off, so the flag users reached for to get past a *privilege guess* also switched off the backup-mismatch check, which is a data-loss decision — hence fixing this together with [F-R4]. `--force` no longer exists; the mismatch override is now `--ignore-backup-mismatch`. Docs and all nine shell call sites updated. **Note:** `check_root()` now has no production callers, surviving only as a `pub` library helper pinned by `public_api_test`; removing it is an API break and was left out of scope, in the same category as [F-R33]/[F-R34]. Verified: 121 unit+contract, 15 integration, 8 durability, `test-resize.sh`, `fmt --check` and `clippy -D warnings` all clean. No safety-net assertion was edited — only the `run_resize` helper's argv.
- **Status date:** 2026-08-01 12:34 UTC
- **Status commit:** d78205f

### [F-R47] Both `unsafe` blocks lack any soundness argument
- **Category:** 30 (security as an afterthought)
- **Impact:** Low
- **Evidence:** `src/system.rs:54` `unsafe { libc::geteuid() == 0 }` and `:75` `unsafe { libc::ioctl(fd, BLKGETSIZE64, &mut size) }`; `grep 'SAFETY' src/` → 0 hits. The ioctl passes a raw `&mut u64` and relies on `:72-73` `#[allow(overflowing_literals)] const BLKGETSIZE64: libc::Ioctl = 0x80081272u32 as libc::Ioctl;` to paper over the glibc/musl `Ioctl` type difference.
- **Found by:** Security & Code Quality

### [F-R48] `get_block_device_size` — the only correct block-device sizing path — is dead; `Device` uses seek
- **Category:** 29 ("utility" dumping ground) / 31
- **Impact:** Low
- **Evidence:** `src/system.rs:58-98` — verified by grep: appears only at its two `#[cfg]` definitions (`:59, :89`), the `pub use` in `lib.rs:11`, and its own unit test (`:126-133`). `Device::open_impl` sizes block devices with `src/device.rs:45` `f.seek(SeekFrom::End(0))?`. The crate's riskiest `unsafe` block sits in code the tool never executes.
- **Found by:** Security & Code Quality, Error Handling & Observability
- **Status:** Fixed
- **Status reason:** `system::block_device_size_of(&File)` now performs the `BLKGETSIZE64` ioctl on an already-open descriptor, and `Device::size_of` calls it for every non-regular file, so the canonical block-device sizing path is what the tool actually runs. Taking an open `File` rather than a path keeps the single `open()` that [F-R23] established -- re-opening by path here would have added a TOCTOU window next to [F-R27]. `get_block_device_size(path)` remains as a thin wrapper over it, so its existing test is unchanged. The `unsafe` block also gained a SAFETY argument, which closes half of [F-R47]; the `geteuid` block still lacks one, so [F-R47] stands.
- **Caveat on coverage:** the ioctl's *success* path is still untested -- nothing in the suite operates on a real block device (the QEMU harness runs the tool against a regular file and uses loop devices only to mount and verify). What is covered is the fallback: `device_open_and_get_block_device_size_agree_on_the_same_path` drives a regular file through `block_device_size_of`, where the ioctl returns -1 and `seek_size` answers, and asserts the result matches `Device`'s own sizing.
- **Status date:** 2026-07-28 00:00 UTC
- **Status commit:** 7799c9b

### [F-R49] Filesystem-type-string validation is an empty `if` body
- **Category:** 20 (weak error handling strategy)
- **Impact:** Low
- **Evidence:** `src/fat32/validation.rs:124-129`, excerpt: `if !fs_type.starts_with(b"FAT32") && !fs_type.starts_with(b"FAT") { // This is a warning-level issue, not an error / // Some tools don't set this correctly }` — the condition is computed and discarded, and no warning channel exists, so a filesystem whose type string is not "FAT…" passes with no signal.
- **Found by:** Error Handling & Observability

### [F-R51] Checkpoint (de)serialization uses hand-written byte offsets on both sides
- **Category:** 28 (magic numbers/strings)
- **Impact:** Low
- **Evidence:** `src/resize/executor.rs:117-195` — `to_bytes` and `from_bytes` each spell out `data[12..16]`, `data[20..24]`, `data[28..32]` as literals, duplicating the layout in two places with only `DATA_SIZE` (`:97`) named. Excerpt: `data[20..24].copy_from_slice(&self.old_fat_size.to_le_bytes());` vs `let old_fat_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);`. A mismatch corrupts the crash-recovery record — and per [F-R11] nothing tests the round-trip.
- **Found by:** Error Handling & Observability

### [F-R52] Spec constants duplicated as bare literals despite named constants existing
- **Category:** 28 (magic numbers/strings)
- **Impact:** Low
- **Evidence:** `src/fat32/validation.rs:20` `if sig != 0xAA55 {` although `BootSector::VALID_SIGNATURE` exists (`structs.rs:332`) and is used at `:336, :348`; `:33` `if !matches!(bps, 512 | 1024 | 2048 | 4096)` re-lists what `operations.rs:12` holds as `VALID_SECTOR_SIZES`; `65525` appears bare in both `validation.rs:117` and `calculator.rs:90`.
- **Found by:** Error Handling & Observability

### [F-R53] Verbose output split between stdout and stderr with no rule
- **Category:** 24 (inconsistent API contracts) / 34
- **Impact:** Low
- **Evidence:** The same `--verbose` flag emits filesystem state on stdout (`src/main.rs:156-158` `println!`) and the resize plan on stderr (`src/resize/executor.rs:631-640` `print_verbose_resize_info`); `:356` `eprintln!("Resuming interrupted resize from phase {:?}…")` goes to stderr unconditionally regardless of verbosity. Redirecting either stream loses half the record. No `--json`/`--quiet`; `FSInfoReport` is exposed only as `Display`, so scripts must parse prose.
- **Found by:** Error Handling & Observability, Integration & Data

### [F-R54] A hand-rolled calendar library lives in the CLI entry point
- **Category:** 11 (low cohesion)
- **Impact:** Low
- **Evidence:** `src/main.rs:10-73` — `const_parse_u64`, `format_build_time`, `days_to_ymd` (leap-year rules and two month-length tables) and `is_leap_year` occupy 64 of main.rs's 273 lines and serve only the `version` subcommand's build-timestamp string. Excerpt: `let days_in_months: [i64; 12] = if is_leap_year(year) { [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31] } else { … };`
- **Found by:** Structure & Boundaries

### [F-R56] `clear_checkpoint` unconditionally zeroes the device's last sector even when no checkpoint was written
- **Category:** 12 (hidden side effects)
- **Impact:** Low
- **Evidence:** `src/resize/executor.rs:609` `clear_checkpoint(&device)?;` sits inside only `if !options.is_dry_run() {`, so it runs on the no-FAT-growth path too, where `write_checkpoint` was never called (all sites are inside `if calculation.fat_needs_growth {` — [F-R26]). Safe today only because the last sector is always inside newly added free space; nothing in the code enforces that invariant.
- **Found by:** Error Handling & Observability
- **Status:** Skipped
- **Status reason:** Deferred after validation established it is **latent and not reachable by test**, in the same sense as [F-R25]. The code is exactly as described — `clear_checkpoint` is outside every `fat_needs_growth` guard — and the intended fix is small (track whether a checkpoint was written, initialising the flag from `incomplete_resize.is_some()` so a resume still clears its predecessor's record, and skip the erase otherwise). It was not applied because the branch it changes cannot currently be entered, so it would have shipped with no test and no way to write one.
- **Status date:** 2026-07-31 00:00 UTC
- **Blocking finding — the no-FAT-growth path appears unreachable:** `fat_needs_growth` is `new_fat_size > old_fat_size` (`calculator.rs:118`), and on `mkfs.fat`-produced images the FAT is sized with no usable slack, so **every** growth recomputes a larger FAT. Measured against a 128 MB image at increments of +4 KB, +16 KB, +64 KB, +256 KB, +512 KB, +1 MB, +4 MB, +8 MB, +12 MB and +22 MB: `FAT tables grew: true` in all ten cases. No reachable input was found that takes the no-growth branch. Whether that is intrinsic, or an artifact of the calculator recomputing `new_fat_size` with different rounding than `mkfs.fat` used, was **not** determined and is worth its own investigation — if it is the latter, the tool grows the FAT on resizes that do not need it, which would be a finding in its own right.
- **Consequent test-coverage finding (pre-dates this session):** because that branch is unreachable, **`tests/integration_test.rs::test_resize_without_fat_growth` does not test what its name says.** It extends 128 MB → 150 MB, comments "small increase, shouldn't require FAT growth", and never asserts `fat_grew` — the run measured above shows that resize takes the growth path. The same misreading is embedded in [F-R26]'s evidence and in this session's own safety-net test `no_valid_checkpoint_survives_a_resize_without_fat_growth`, which passes but exercises the growth path. **No test in the suite covers the no-growth path.** A future session should either make the path reachable and test it, or establish that it is dead and delete it along with the tests that claim to cover it; either way the misleading names should go. Asserting `result.fat_grew` in these tests would have caught this years earlier and costs one line.

### [F-R57] fault-injection build state is not visible at runtime
- **Category:** 22 (configuration sprawl)
- **Impact:** Low *(corrected down from Medium: the release workflow builds default features, so such a binary is never distributed)*
- **Evidence:** `src/main.rs:75-82` `version_long()` formats only `CARGO_PKG_VERSION`, `format_build_time()` and `GIT_HASH` — nothing distinguishes a `--features fault-injection` binary (which will `std::process::exit(137)` mid-resize when `FAT32_CRASH_AT` is set, `executor.rs:29-37`) from a production one.
- **Found by:** Error Handling & Observability

### [F-R55] Version-string helpers use unchecked arithmetic and `unwrap()`
- **Category:** 20 (weak error handling strategy)
- **Impact:** Low
- **Evidence:** `src/main.rs:15` `result = result * 10 + (bytes[i] - b'0') as u64;` with no digit check; `:23` `.unwrap()` on `duration_since`. The only `unwrap`/panic sites in non-test code.
- **Found by:** Error Handling & Observability
- **Verdict:** **PLAUSIBLE, not confirmed** — the code is exactly as described, but neither can panic at runtime: `const_parse_u64` is const-evaluated (`const BUILD_TIMESTAMP: u64 = const_parse_u64(env!("BUILD_TIMESTAMP"))`, `:7`), so malformed input is a compile error, and `(UNIX_EPOCH + d).duration_since(UNIX_EPOCH)` cannot fail. "Panicking helpers" overstates the risk.

## Coverage Checklist

### Flaw/Risk Types 1–34

| # | Type | Status | Finding |
|---|------|--------|---------|
| 1 | Global mutable state | Not observed | — (the one candidate, env-var fault injection, is compiled out of shipped builds — see [S-E]) |
| 2 | God object | Observed | [F-R14] |
| 3 | Tight coupling | Not observed | — (candidate resolved into [S-Q]) |
| 4 | High/unstable dependencies | Not observed | — (see [S-J]) |
| 5 | Circular dependencies | Not observed | — (see [S-A]) |
| 6 | Leaky abstractions | Observed | [F-R44] |
| 7 | Over-abstraction | Not observed | — ([F-R35] classified as unused typing, not over-layering) |
| 8 | Premature optimization | Observed | [F-R58] |
| 9 | Shotgun surgery | Observed | [F-R13], [F-R39], [F-R43] |
| 10 | Feature envy / anemic domain model | Not observed | — |
| 11 | Low cohesion | Observed | [F-R42], [F-R54] |
| 12 | Hidden side effects | Observed | [F-R40], [F-R24], [F-R32], [F-R56] |
| 13 | Inconsistent boundaries | Observed | [F-R35], [F-R37], [F-R5] |
| 14 | Distributed monolith | Not applicable | Single binary + library, one process — nothing distributed |
| 15 | Chatty service calls | Not applicable | No inter-service calls; sector I/O chattiness is a performance matter ([F-R58]) |
| 16 | Synchronous-only integration | Not applicable | One-shot CLI mutating a block device; no integration boundary that could be async |
| 17 | No clear ownership of data | Observed | [F-R2], [F-R45] |
| 18 | Shared database across services | Not applicable | No database, no second service; the device has exactly one writer |
| 19 | Lack of idempotency | Observed | [F-R1], [F-R3] |
| 20 | Weak error handling strategy | Observed | [F-R5], [F-R29], [F-R30], [F-R36], [F-R49], [F-R55] |
| 21 | No observability plan | Observed | [F-R19], [F-R20], [F-R21] |
| 22 | Configuration sprawl | Observed | [F-R57] |
| 23 | Dependency injection misuse | Observed | [F-R38] |
| 24 | Inconsistent API contracts | Observed | [F-R4], [F-R31], [F-R32], [F-R53] |
| 25 | Business logic in the UI | Observed | [F-R41] |
| 26 | Poor transactional boundaries | Observed | [F-R17], [F-R18], [F-R25], [F-R26] |
| 27 | Temporal coupling | Observed | [F-R15], [F-R23] |
| 28 | Magic numbers/strings everywhere | Observed | [F-R36], [F-R51], [F-R52] |
| 29 | "Utility" dumping ground | Observed | [F-R33], [F-R34], [F-R48] |
| 30 | Security as an afterthought | Observed | [F-R6], [F-R7], [F-R8], [F-R27], [F-R28], [F-R46], [F-R47] |
| 31 | Dead code / unused dependencies | Observed | [F-R33], [F-R34], [F-R35], [F-R44], [F-R48] |
| 32 | Missing test coverage for critical paths | Observed | [F-R9], [F-R10], [F-R11], [F-R12] |
| 33 | Hard-coded credentials or secrets | Not observed | — (see [S-R]) |
| 34 | Inconsistent error/logging conventions | Observed | [F-R16], [F-R30], [F-R53], [F-R60] |

### Strength Categories S1–S14

| # | Category | Status | Finding |
|---|----------|--------|---------|
| S1 | Clear modular boundaries | Observed | [S-A] |
| S2 | High cohesion | Observed | [S-H] |
| S3 | Loose coupling | Observed | [S-B], [S-G], [S-K], [S-E] |
| S4 | Dependency direction is stable | Observed | [S-A] |
| S5 | Dependency management hygiene | Observed | [S-J] |
| S6 | Consistent API contracts | Observed | [S-K], [S-L] |
| S7 | Robust error handling | Observed | [S-B], [S-C], [S-N], [S-P] |
| S8 | Observability present | Observed | [S-I] |
| S9 | Configuration discipline | Observed | [S-E], [S-O] |
| S10 | Security built-in | Observed | [S-C], [S-H], [S-J], [S-R] |
| S11 | Testability & coverage | Observed | [S-D], [S-E], [S-Q] |
| S12 | Resilience patterns | Observed | [S-C], [S-E], [S-P] |
| S13 | Domain modeling strength | Observed | [S-F], [S-M] |
| S14 | Simple, pragmatic abstractions | Observed | [S-C], [S-O] |

## Hotspots

1. **`src/resize/relocator.rs`** — 256 lines carrying the two reproduced data-loss defects. `plan_relocation` owns a second, cluster-rounded definition of where the data area starts ([F-R2]); `execute_relocation` is safe forwards but not re-runnable ([F-R1]); `verify_relocation` is a no-op the executor reports as a completed check ([F-R20]); and three of its entry points take parameters they discard ([F-R38]). No test exercises any of its arithmetic ([F-R10]). Highest risk-to-line-count ratio in the repository.

2. **`src/resize/executor.rs`** — 885 lines, five concerns ([F-R42]), and a 307-line orchestration function ([F-R14]) inside which the crash-safety protocol is enforced by comments and integer comparisons rather than types ([F-R15]). Both sync-barrier gaps ([F-R17], [F-R18]) and both checkpoint-coverage gaps ([F-R25], [F-R26]) live here, as does the duplicated layout arithmetic that [F-R2] descends from ([F-R13]). Also the strongest code in the project — the checkpoint design ([S-C]) is genuinely good.

3. **`src/fat32/validation.rs` + `structs.rs:391-404`** — the trust boundary for untrusted on-disk input. Validation is correctly wired in ([S-H]) but checks only individual fields ([F-R8]), and its one cross-field check consumes a value that can silently wrap ([F-R7]), which is what lets a crafted image be reported as healthy or abort the process ([F-R6]). Three rejection tests and no fuzzing ([F-R12]).

Worth noting alongside these: **`scripts/qemu/test-qemu.sh:404`** is a one-line defect with outsized consequences — the glob that silently excludes the crash-recovery suite from CI ([F-R9]) is the reason [F-R1] survived.

## Next Questions

1. Which of the two definitions of "new first data sector" is intended to be authoritative — the cluster-rounded one in `plan_relocation` or the un-rounded one implied by the committed boot sector — and is there a geometry in the wild (EFI system partitions on 4Kn drives, say) where the divergence in [F-R2] is common rather than rare?
2. Is `test-crash-recovery.sh` outside `scripts/qemu/tests/` deliberately (because it needs its own build with `--features fault-injection`), and if so what was meant to invoke it in CI?
3. Should resume from phase `Started` be safe to re-run at all, or should the checkpoint record how far the shift progressed so a resume can continue rather than restart?
4. Was the `ClusterId`/`SectorNum` work in commit 238b305 intended as the first step of a migration that stalled, or as documentation of an intended future direction?
5. What is the intended contract for the library's public surface — is `fat32expander::fat32::structs::BootSector` supported API, or should the crate expose only the facade that `main.rs` and the integration tests actually use?

## Analysis Metadata

- **Agents dispatched:** 6 — Structure & Boundaries (module organization, responsibility distribution, domain modeling); Coupling & Dependencies (component connections, abstraction quality, dependency direction); Integration & Data (data ownership, idempotency, transactional boundaries, API contracts); Error Handling & Observability (error strategy, logging, config, side effects); Security & Code Quality (untrusted input, privilege, dead code, test coverage); plus a Verifier.
- **Scope:** 15 Rust source files (~3,700 LOC) + `build.rs` + `tests/integration_test.rs` + ~14 shell scripts + 2 GitHub workflows + 3 steering documents.
- **Raw findings:** 78 (60 flaws, 18 strengths)
- **Verified findings:** 75 (57 flaws, 18 strengths)
- **Filtered out:** 3 — 1 dropped as double-counting (fault-injection `process::exit`, superseded by [S-E]); 1 merged (unchecked-arithmetic survey folded into [F-R7]); 1 flaw resolved into a strength after reconciling a direct contradiction between two specialists ([S-Q]).
- **By impact:** flaws — 7 High, 33 Medium, 17 Low; strengths — 6 High, 11 Medium, 1 Low.
- **Verdicts:** 74 CONFIRMED, 1 PLAUSIBLE ([F-R55]).
- **Impact corrections applied during verification:** 12 (11 High→Medium, 1 Medium→Low). High was reserved for findings that cause or conceal silent data corruption.
- **Empirically reproduced:** 4 findings — [F-R1] and [F-R2] against real FAT32 images built with `mkfs.fat` and checked with `fsck.fat` and SHA-256 diffs; [F-R6] and [F-R7] against crafted boot sectors.
- **Steering files consulted:** `CLAUDE.md`, `TECHGUIDE.md`, `README.md` — two contradictions found ([F-R3] TECHGUIDE's idempotency claim, [F-R60] CLAUDE.md's abandoned algorithm).
