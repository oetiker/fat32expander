//! Safety net for the resize phase machinery: [F-R14], [F-R15].
//!
//! The crash-safe sequence in `resize::executor` is guarded by
//! `if starting_phase == ResizePhase::Started` and
//! `if starting_phase <= ResizePhase::DataCopied`. Those two branches decide
//! which of the three phases a resumed run redoes and which it skips, and they
//! are the code any restructuring of the phase machinery replaces.
//!
//! Before this file existed, nothing `cargo test` runs entered them. Their only
//! coverage was `scripts/qemu/test-crash-recovery.sh`, which needs an Alpine VM
//! built by `setup-alpine.sh` -- so a refactor of the phase logic could go green
//! locally and in CI while silently breaking every resume path. The QEMU harness
//! is not redundant (it exercises real block devices and a real kill), but it is
//! not a net a refactor can lean on either.
//!
//! No QEMU is needed to reach these branches: the fault-injection hook exits the
//! process with 137, and on an image file that is indistinguishable from the
//! crash the harness stages. The CLI is driven as a subprocess for exactly that
//! reason -- the crash point must terminate a process that is not the test
//! runner.
//!
//! # What is asserted, and what deliberately is not
//!
//! These pin *observable* outcomes: the exit codes, the user-facing "Resuming
//! interrupted resize from phase X" line, filesystem integrity under `dosfsck`,
//! byte-identical file contents, and geometry equal to that of an uninterrupted
//! resize.
//!
//! They do **not** assert on the "Skipping data shift (already done)" audit
//! strings, even though those name the branches under test. Data integrity is
//! the stronger check and the stable one: the data shift is deliberately
//! non-idempotent ([F-R1], [F-R3]), so a resume that wrongly redoes it copies
//! already-shifted clusters over themselves and the file contents no longer
//! match. "The files are intact" therefore proves the phase was skipped, without
//! pinning the wording of a log line a refactor is free to change.
//!
//! Requires the `fault-injection` feature plus mkfs.fat, dosfsck and mtools:
//!
//! ```text
//! cargo test --features fault-injection --test crash_recovery_test -- --ignored
//! ```

#![cfg(feature = "fault-injection")]

use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

const BIN: &str = env!("CARGO_BIN_EXE_fat32expander");

/// The fault-injection hook exits with this; see `resize::fault`.
const CRASH_EXIT: i32 = 137;

/// Files written into every image before the resize, as (name, size in bytes).
///
/// Sizes are deliberately spread across the cluster size (512 bytes at the
/// geometry these tests use) so the set includes sub-cluster, exactly-one-
/// cluster and multi-cluster files. A shift replayed over its own output
/// corrupts the tail of a multi-cluster file first, which a single small file
/// would not reveal.
const FILES: &[(&str, usize)] = &[
    ("small.txt", 100),
    ("exact.txt", 512),
    ("multi.txt", 5000),
    ("large.txt", 40000),
];

/// Deterministic, position-dependent bytes, so a chunk copied from the wrong
/// offset does not happen to match.
fn content(name: &str, len: usize) -> Vec<u8> {
    let seed = name.bytes().map(|b| b as usize).sum::<usize>();
    (0..len)
        .map(|i| ((i.wrapping_mul(31).wrapping_add(seed * 7)) % 251) as u8)
        .collect()
}

fn run(cmd: &mut Command) -> std::process::Output {
    cmd.output().expect("failed to spawn command")
}

/// A `size_mb` FAT32 image carrying `FILES`, in a file grown to `grown_mb` so
/// there is room to resize into.
fn image_with_files(size_mb: u32, grown_mb: u32) -> NamedTempFile {
    let file = NamedTempFile::new().expect("temp file");
    truncate(file.path(), size_mb);

    let status = Command::new("mkfs.fat")
        .args(["-F", "32"])
        .arg(file.path())
        .status()
        .expect("failed to run mkfs.fat");
    assert!(status.success(), "mkfs.fat failed");

    for (name, len) in FILES {
        let src = NamedTempFile::new().expect("temp file");
        std::fs::write(src.path(), content(name, *len)).expect("write source file");
        let out = run(Command::new("mcopy")
            .arg("-i")
            .arg(file.path())
            .arg(src.path())
            .arg(format!("::{}", name)));
        assert!(
            out.status.success(),
            "mcopy of {} failed: {}",
            name,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    truncate(file.path(), grown_mb);
    file
}

fn truncate(path: &Path, size_mb: u32) {
    let status = Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", size_mb))
        .arg(path)
        .status()
        .expect("failed to run truncate");
    assert!(status.success(), "truncate failed");
}

/// Resize `path`, crashing at `crash_point`. Asserts the process really died
/// there -- a crash point that stopped being reached would otherwise turn the
/// test into an uninterrupted resize that passes for the wrong reason.
fn resize_crashing_at(path: &Path, crash_point: &str) {
    let out = run(Command::new(BIN)
        .env("FAT32_CRASH_AT", crash_point)
        .arg("resize")
        .arg(path));
    assert_eq!(
        out.status.code(),
        Some(CRASH_EXIT),
        "expected a crash at {} (exit {}), got {:?}\nstdout: {}\nstderr: {}",
        crash_point,
        CRASH_EXIT,
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

/// Resize `path` to completion, returning stdout and stderr joined.
fn resize_expecting_success(path: &Path) -> String {
    let out = run(Command::new(BIN).arg("resize").arg(path));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "resize failed with {:?}:\n{}",
        out.status.code(),
        text
    );
    text
}

fn assert_filesystem_is_clean(path: &Path) {
    let out = run(Command::new("dosfsck").arg("-n").arg(path));
    assert!(
        out.status.success(),
        "dosfsck reported a damaged filesystem:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Every file in `FILES` still reads back byte for byte.
///
/// This is what proves a resumed run did not replay a phase it should have
/// skipped: the data shift walks clusters downward overwriting as it goes, so
/// running it twice reads sources the first pass already clobbered.
fn assert_files_are_intact(path: &Path) {
    for (name, len) in FILES {
        let dest = NamedTempFile::new().expect("temp file");
        let out = run(Command::new("mcopy")
            .args(["-n", "-i"])
            .arg(path)
            .arg(format!("::{}", name))
            .arg(dest.path()));
        assert!(
            out.status.success(),
            "{} could not be read back: {}",
            name,
            String::from_utf8_lossy(&out.stderr)
        );
        let got = std::fs::read(dest.path()).expect("read extracted file");
        let want = content(name, *len);
        assert_eq!(
            got.len(),
            want.len(),
            "{} came back {} bytes, expected {}",
            name,
            got.len(),
            want.len()
        );
        assert!(
            got == want,
            "{} came back with different contents -- a phase was replayed over \
             its own output",
            name
        );
    }
}

/// The phase named in the user-facing resume line.
fn resumed_phase(output: &str) -> String {
    let marker = "Resuming interrupted resize from phase ";
    let start = output
        .find(marker)
        .unwrap_or_else(|| panic!("no resume line in output:\n{}", output))
        + marker.len();
    output[start..]
        .split(['.', '\n'])
        .next()
        .expect("phase name")
        .trim()
        .to_string()
}

/// `info` output, used to compare the geometry two runs arrived at.
fn geometry(path: &Path) -> String {
    let out = run(Command::new(BIN).arg("info").arg(path));
    assert!(out.status.success(), "info failed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| {
            l.contains("Total sectors")
                || l.contains("FAT size")
                || l.contains("Data clusters")
                || l.contains("Sectors per cluster")
                || l.contains("Reserved sectors")
                || l.contains("Free clusters")
        })
        .map(|l| l.trim().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Crash at `crash_point`, resume, and assert the filesystem came through
/// whole. Returns the phase the resumed run reported starting from.
fn crash_then_resume(crash_point: &str) -> String {
    let image = image_with_files(128, 200);
    resize_crashing_at(image.path(), crash_point);

    let output = resize_expecting_success(image.path());
    let phase = resumed_phase(&output);

    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
    phase
}

/// Crash at `crash_point`, then dry-run against the interrupted filesystem.
/// The dry run must not write a single byte, and the real run after it must
/// still resume from `phase` and finish with the files intact.
fn dry_run_after_crash_changes_nothing(crash_point: &str, phase: &str) {
    let image = image_with_files(128, 200);
    resize_crashing_at(image.path(), crash_point);
    let before = std::fs::read(image.path()).expect("read image");

    // The exit code is not pinned: what a dry run reports here is what [F-21]
    // changes. That it leaves the device alone is not.
    run(Command::new(BIN)
        .arg("resize")
        .arg("--dry-run")
        .arg(image.path()));
    assert!(
        std::fs::read(image.path()).expect("read image") == before,
        "a dry run after a crash at {crash_point} changed the device"
    );

    let output = resize_expecting_success(image.path());
    assert_eq!(resumed_phase(&output), phase);
    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
}

/// Safety net for [F-21], before the boot sector is invalidated.
#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_dry_run_after_an_early_crash_changes_nothing() {
    dry_run_after_crash_changes_nothing("after_checkpoint_start", "Started");
}

/// Safety net for [F-21], inside the danger zone.
#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_dry_run_inside_the_danger_zone_changes_nothing() {
    dry_run_after_crash_changes_nothing("after_boot_invalidate", "DataCopied");
}

/// The "New size:" line of a resize summary.
fn new_size_line(output: &str) -> String {
    output
        .lines()
        .find(|l| l.contains("New size:"))
        .unwrap_or_else(|| panic!("no New size line in output:\n{}", output))
        .trim()
        .to_string()
}

/// [F-21]: a dry run after a crash previewed a fresh resize built from the
/// boot sector the crash left behind. It must preview the resume the real
/// run then performs: same phase, same new size.
#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_dry_run_after_a_crash_previews_the_resume() {
    let image = image_with_files(128, 200);
    resize_crashing_at(image.path(), "after_boot_invalidate");

    let dry = run(Command::new(BIN)
        .arg("resize")
        .arg("--dry-run")
        .arg(image.path()));
    let dry_text = format!(
        "{}{}",
        String::from_utf8_lossy(&dry.stdout),
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(
        dry.status.success(),
        "the dry run failed with {:?}:\n{}",
        dry.status.code(),
        dry_text
    );
    assert!(
        dry_text.contains("would be resumed from phase DataCopied"),
        "the dry run must name the phase it would resume from:\n{}",
        dry_text
    );

    let real = resize_expecting_success(image.path());
    assert_eq!(resumed_phase(&real), "DataCopied");
    assert_eq!(new_size_line(&dry_text), new_size_line(&real));
}

// --- One test per injectable crash point ------------------------------------
//
// The phase each one resumes from is asserted because it is the contract the
// two `starting_phase` guards implement: a crash before the phase-1 checkpoint
// must come back as `Started` and redo the shift, one after it must come back
// as `DataCopied` and not.

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_right_after_the_first_checkpoint_resumes_from_started() {
    assert_eq!(crash_then_resume("after_checkpoint_start"), "Started");
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_after_the_data_shift_resumes_from_started() {
    // The shift is done but unrecorded, so recovery must treat it as
    // unfinished and replay it from the progress record -- not from the top.
    assert_eq!(crash_then_resume("after_data_shift"), "Started");
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_after_the_data_copied_checkpoint_resumes_from_data_copied() {
    assert_eq!(
        crash_then_resume("after_checkpoint_data_copied"),
        "DataCopied"
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_inside_the_danger_zone_resumes_from_data_copied() {
    // Boot signature invalidated, FAT not yet written: recovery has to enter
    // the danger zone again and must not redo the shift underneath it.
    assert_eq!(crash_then_resume("after_boot_invalidate"), "DataCopied");
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_after_the_fat_write_resumes_from_data_copied() {
    // The FAT is on disk but the checkpoint does not say so yet, so the FAT
    // work is redone. It must be idempotent for that to be safe.
    assert_eq!(crash_then_resume("after_fat_write"), "DataCopied");
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_after_the_fat_written_checkpoint_resumes_from_fat_written() {
    // Both earlier phases skipped; only the metadata commit remains.
    assert_eq!(
        crash_then_resume("after_checkpoint_fat_written"),
        "FatWritten"
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_before_the_primary_boot_write_resumes_from_fat_written() {
    // Safety net for [F-04]: the commit itself is interrupted, after the
    // backup boot sector and FSInfo landed but before the primary did. The
    // FatWritten checkpoint is still on disk, so the commit runs again.
    assert_eq!(crash_then_resume("before_primary_boot_write"), "FatWritten");
}

/// A file large enough that the shift records its progress more than once.
///
/// The shift records progress at most every 1024 clusters. `FILES` fill about
/// 90 clusters of 512 bytes, so their shift has only its first record and
/// `before_mid_shift_record` never fires. This file adds about 2900 clusters.
const LONG_FILE: (&str, usize) = ("long.bin", 1_500_000);

/// `image_with_files(128, 200)` plus `LONG_FILE`.
fn image_with_a_long_shift() -> NamedTempFile {
    let image = image_with_files(128, 200);
    let (name, len) = LONG_FILE;
    let src = NamedTempFile::new().expect("temp file");
    std::fs::write(src.path(), content(name, len)).expect("write source file");
    let out = run(Command::new("mcopy")
        .arg("-i")
        .arg(image.path())
        .arg(src.path())
        .arg(format!("::{}", name)));
    assert!(
        out.status.success(),
        "mcopy of {} failed: {}",
        name,
        String::from_utf8_lossy(&out.stderr)
    );
    image
}

/// `LONG_FILE` still reads back byte for byte.
fn assert_long_file_is_intact(path: &Path) {
    let (name, len) = LONG_FILE;
    let dest = NamedTempFile::new().expect("temp file");
    let out = run(Command::new("mcopy")
        .args(["-n", "-i"])
        .arg(path)
        .arg(format!("::{}", name))
        .arg(dest.path()));
    assert!(out.status.success(), "{} could not be read back", name);
    assert!(
        std::fs::read(dest.path()).expect("read extracted file") == content(name, len),
        "{} came back with different contents",
        name
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_in_the_middle_of_the_data_shift_resumes_from_started() {
    // [F-T08]: the crash comes after the moves since the first progress
    // record are on disk, but before the next record covers them. The resume
    // must redo those moves from the first record without reading a source
    // the first pass already overwrote, and land where an uninterrupted
    // resize lands.
    let reference = image_with_a_long_shift();
    resize_expecting_success(reference.path());
    let expected = geometry(reference.path());

    let image = image_with_a_long_shift();
    resize_crashing_at(image.path(), "before_mid_shift_record");
    let output = resize_expecting_success(image.path());

    assert_eq!(resumed_phase(&output), "Started");
    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
    assert_long_file_is_intact(image.path());
    assert_eq!(geometry(image.path()), expected);
}

/// An image with `FILES` on it that a resize grows without growing the FAT.
///
/// A fresh `mkfs.fat` image always grows the FAT, so this one is resized once
/// first, from 128 MiB to 129 MiB, and then grown by 32 sectors: the FAT the
/// first resize sized still covers them (see `integration_test.rs`).
fn image_ready_for_a_resize_without_fat_growth() -> NamedTempFile {
    let image = image_with_files(128, 129);
    resize_expecting_success(image.path());
    let status = Command::new("truncate")
        .args(["-s", "132112K"]) // 264224 sectors
        .arg(image.path())
        .status()
        .expect("failed to run truncate");
    assert!(status.success(), "truncate failed");
    image
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_inside_a_commit_without_fat_growth_is_recoverable() {
    // Safety net for [F-04]: without FAT growth no checkpoint is written, so
    // a crash between the backup and the primary boot sector leaves only the
    // two disagreeing boot sectors. However the next run gets there, the
    // result must equal an uninterrupted resize. How the plain rerun answers
    // is not pinned: that is what [F-04] changes. The documented escape,
    // `--ignore-backup-mismatch`, must work.
    let reference = image_ready_for_a_resize_without_fat_growth();
    let output = resize_expecting_success(reference.path());
    assert!(
        output.contains("FAT tables grew: false") && !output.contains("Resuming"),
        "fixture drifted: the reference resize must neither resume nor grow \
         the FAT:\n{}",
        output
    );
    let expected = geometry(reference.path());

    let image = image_ready_for_a_resize_without_fat_growth();
    resize_crashing_at(image.path(), "before_primary_boot_write");
    let out = run(Command::new(BIN)
        .arg("resize")
        .arg(image.path())
        .arg("--ignore-backup-mismatch"));
    assert!(
        out.status.success(),
        "the override rerun failed with {:?}:\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
    assert_eq!(
        geometry(image.path()),
        expected,
        "a commit rerun after a crash produced different geometry than an \
         uninterrupted one"
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn an_interrupted_sub_cluster_shift_exits_9() {
    // [F-06]: the refusal in `an_interrupted_sub_cluster_shift_refuses_to_resume`
    // is a filesystem that needs recovery, so it exits 9, not 1. Same fixture.
    let image = NamedTempFile::new().expect("temp file");
    truncate(image.path(), 2200);
    let status = Command::new("mkfs.fat")
        .args(["-F", "32", "-s", "64"])
        .arg(image.path())
        .status()
        .expect("failed to run mkfs.fat");
    assert!(status.success(), "mkfs.fat failed");
    truncate(image.path(), 2308);

    resize_crashing_at(image.path(), "after_checkpoint_start");

    let out = run(Command::new(BIN).arg("resize").arg(image.path()));
    assert_eq!(
        out.status.code(),
        Some(9),
        "a shift with no progress record needs recovery:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_inside_a_commit_without_fat_growth_resumes_on_its_own() {
    // [F-04]: the commit without FAT growth leaves a record too, so a plain
    // rerun finishes it. No `--ignore-backup-mismatch` needed.
    let reference = image_ready_for_a_resize_without_fat_growth();
    resize_expecting_success(reference.path());
    let expected = geometry(reference.path());

    let image = image_ready_for_a_resize_without_fat_growth();
    resize_crashing_at(image.path(), "before_primary_boot_write");
    let output = resize_expecting_success(image.path());

    assert_eq!(resumed_phase(&output), "FatWritten");
    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
    assert_eq!(geometry(image.path()), expected);
}

// --- The device changes size between the crash and the rerun [F-03] -------
//
// The record of an interrupted resize sits at the last sector the device had
// when the resize started. A rerun must find it there whatever size the
// device has now, or it plans a fresh shift over data the first run already
// moved.

/// Crash at `crash_point`, change the image to `rerun_mb`, and resume.
fn crash_resize_to_then_resume(crash_point: &str, rerun_mb: u32) -> String {
    let image = image_with_files(128, 200);
    resize_crashing_at(image.path(), crash_point);
    truncate(image.path(), rerun_mb);

    let output = resize_expecting_success(image.path());
    let phase = resumed_phase(&output);

    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
    // The resume completes the resize it was interrupted in; a second run
    // takes the filesystem the rest of the way.
    resize_expecting_success(image.path());
    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
    phase
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_in_the_data_shift_resumes_after_the_device_grew() {
    assert_eq!(
        crash_resize_to_then_resume("after_data_shift", 250),
        "Started"
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_in_the_danger_zone_resumes_after_the_device_grew() {
    assert_eq!(
        crash_resize_to_then_resume("after_boot_invalidate", 250),
        "DataCopied"
    );
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_crash_in_the_data_shift_is_refused_after_the_device_shrank() {
    let image = image_with_files(128, 200);
    resize_crashing_at(image.path(), "after_data_shift");
    truncate(image.path(), 180);
    let before = std::fs::read(image.path()).expect("read image");

    let out = run(Command::new(BIN).arg("resize").arg(image.path()));
    assert_eq!(
        out.status.code(),
        Some(9),
        "a resume onto a device smaller than its plan needs recovery:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read(image.path()).expect("read image");
    assert!(
        before == after,
        "a refused resume must not modify the image"
    );
}

// --- Cross-cutting properties -----------------------------------------------

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_resumed_resize_lands_on_the_same_geometry_as_an_uninterrupted_one() {
    // The sharpest statement of what the phase machinery is for: where the run
    // was interrupted must not be observable in the result. Two images built
    // identically, one resized straight through and one crashed at every
    // injectable point in turn, must end up describing the same filesystem.
    let reference = image_with_files(128, 200);
    resize_expecting_success(reference.path());
    assert_filesystem_is_clean(reference.path());
    let expected = geometry(reference.path());

    for crash_point in [
        "after_checkpoint_start",
        "after_data_shift",
        "after_checkpoint_data_copied",
        "after_boot_invalidate",
        "after_fat_write",
        "after_checkpoint_fat_written",
        "before_primary_boot_write",
    ] {
        let image = image_with_files(128, 200);
        resize_crashing_at(image.path(), crash_point);
        resize_expecting_success(image.path());
        assert_filesystem_is_clean(image.path());
        assert_files_are_intact(image.path());
        assert_eq!(
            geometry(image.path()),
            expected,
            "a resize resumed after {} produced different geometry than an \
             uninterrupted one",
            crash_point
        );
    }
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_completed_resize_leaves_nothing_for_a_second_run_to_resume() {
    let image = image_with_files(128, 200);
    resize_crashing_at(image.path(), "after_boot_invalidate");
    resize_expecting_success(image.path());

    // Already at maximum size, so the second run is a no-op -- and, the point
    // of the test, it is reported as that rather than as another interrupted
    // resize. A checkpoint left behind by the recovered run would send this
    // down the resume path instead.
    let out = run(Command::new(BIN).arg("resize").arg(image.path()));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "a maximal filesystem is a no-op, not a failure:\n{}",
        text
    );
    assert!(
        text.contains("Nothing to do"),
        "the no-op must say so:\n{}",
        text
    );
    assert!(
        !text.contains("Resuming interrupted resize"),
        "a retired checkpoint was still treated as live:\n{}",
        text
    );
    assert_filesystem_is_clean(image.path());
    assert_files_are_intact(image.path());
}

#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn an_interrupted_sub_cluster_shift_refuses_to_resume() {
    // The other side of the `Started` branch. When the whole shift is shorter
    // than one cluster, no move can ever be safely replayed, so the run
    // records no progress -- and a resume that finds a phase-0 checkpoint with
    // no progress record must refuse rather than copy shifted data over
    // itself.
    //
    // Reaching it needs `num_fats * fat_growth < sectors_per_cluster`: a
    // 32 KB-cluster filesystem whose FAT grows by exactly one sector, which
    // needs enough clusters to clear the FAT32 minimum. The image is sparse,
    // so its nominal size costs no disk.
    let image = NamedTempFile::new().expect("temp file");
    truncate(image.path(), 2200);
    let status = Command::new("mkfs.fat")
        .args(["-F", "32", "-s", "64"])
        .arg(image.path())
        .status()
        .expect("failed to run mkfs.fat");
    assert!(status.success(), "mkfs.fat failed");
    truncate(image.path(), 2308);

    resize_crashing_at(image.path(), "after_checkpoint_start");

    let out = run(Command::new(BIN).arg("resize").arg(image.path()));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "resuming a shift with no progress record must fail:\n{}",
        text
    );
    assert!(
        text.contains("no progress record"),
        "the refusal must say why it cannot resume:\n{}",
        text
    );
    // Refusing is only the right answer if it leaves the filesystem alone.
    assert_filesystem_is_clean(image.path());
}

/// The "FAT tables grew:" line of a resize summary.
fn fat_grew_line(output: &str) -> String {
    output
        .lines()
        .find(|l| l.contains("FAT tables grew:"))
        .unwrap_or_else(|| panic!("no FAT tables grew line in output:\n{}", output))
        .trim()
        .to_string()
}

/// Safety net for [F-09]. A resume rebuilds its size calculation from the
/// checkpoint instead of from the boot sector, so the summary it reports must
/// still match the one an uninterrupted run reports.
#[test]
#[ignore = "requires mkfs.fat, dosfsck and mtools"]
fn a_resumed_resize_reports_the_same_summary_as_an_uninterrupted_one() {
    let reference = image_with_files(128, 200);
    let expected = resize_expecting_success(reference.path());

    for crash_point in [
        "after_checkpoint_data_copied",
        "after_checkpoint_fat_written",
    ] {
        let image = image_with_files(128, 200);
        resize_crashing_at(image.path(), crash_point);
        let resumed = resize_expecting_success(image.path());
        assert_eq!(
            new_size_line(&resumed),
            new_size_line(&expected),
            "a resize resumed after {} reported a different new size",
            crash_point
        );
        assert_eq!(
            fat_grew_line(&resumed),
            fat_grew_line(&expected),
            "a resize resumed after {} reported a different FAT growth",
            crash_point
        );
    }
}
