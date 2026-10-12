//! Safety net for the durability-barrier batch: [F-R17], [F-R18], [F-R26],
//! [F-R56].
//!
//! These pin the order in which a resize mutates the device, and what it leaves
//! behind. They exist because a missing sync barrier is invisible to every other
//! harness in this project -- the QEMU crash harness kills the process, after
//! which the page cache flushes everything regardless of what the code asked for
//! -- so nothing else in the suite can tell a durable write sequence from a
//! hopeful one.
//!
//! Requires the `device-journal` feature plus mkfs.fat and dosfsck:
//!
//! ```text
//! cargo test --features device-journal --test durability_test -- --ignored
//! ```

#![cfg(feature = "device-journal")]

use fat32expander::journal::{self, Op};
use fat32expander::{get_fs_info, resize_fat32, Error, ResizeOptions};
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

/// First bytes of a checkpoint record, per `resize::checkpoint`. Spelled out
/// here rather than imported because the checkpoint type is crate-private by
/// [F-R44]'s decision, and an external observer is exactly the right vantage
/// point for "did anything survive on the device".
const CHECKPOINT_MAGIC: &[u8; 8] = b"FAT32RSZ";

fn create_fat32_image(size_mb: u32) -> NamedTempFile {
    let file =
        NamedTempFile::new_in(env!("CARGO_TARGET_TMPDIR")).expect("Failed to create temp file");
    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", size_mb))
        .arg(file.path())
        .status()
        .expect("Failed to truncate file");
    let status = Command::new("mkfs.fat")
        .arg("-F")
        .arg("32")
        .arg(file.path())
        .status()
        .expect("Failed to run mkfs.fat");
    assert!(status.success(), "mkfs.fat failed");
    file
}

fn extend_image(path: &Path, new_size_mb: u32) {
    Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", new_size_mb))
        .arg(path)
        .status()
        .expect("Failed to extend file");
}

fn check_filesystem(path: &Path) -> bool {
    Command::new("dosfsck")
        .arg("-n")
        .arg(path)
        .output()
        .expect("Failed to run dosfsck")
        .status
        .success()
}

fn read_sector(path: &Path, sector: u64, sector_size: u64) -> Vec<u8> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).expect("open image");
    f.seek(SeekFrom::Start(sector * sector_size)).expect("seek");
    let mut buf = vec![0u8; sector_size as usize];
    f.read_exact(&mut buf).expect("read sector");
    buf
}

/// Where the interesting sectors live, read off the filesystem after a resize.
struct Layout {
    sector_size: u64,
    fs_info: u64,
    backup: u64,
    fat_start: u64,
    fat_end: u64,
    checkpoint: u64,
}

fn layout_of(path: &Path) -> Layout {
    let info = get_fs_info(path).expect("read fs info");
    let boot = info.boot();
    let reserved = boot.reserved_sectors() as u64;
    Layout {
        sector_size: boot.bytes_per_sector() as u64,
        fs_info: boot.fs_info_sector() as u64,
        backup: boot.backup_boot_sector() as u64,
        fat_start: reserved,
        fat_end: reserved + boot.num_fats() as u64 * boot.fat_size_32() as u64,
        checkpoint: info.device_sectors() - 1,
    }
}

fn is_write_to(op: &Op, sector: u64) -> bool {
    matches!(op, Op::Write { start, count } if *start <= sector && sector < start + count)
}

fn is_write_within(op: &Op, from: u64, to: u64) -> bool {
    matches!(op, Op::Write { start, .. } if *start >= from && *start < to)
}

/// A resize large enough that the FAT tables must grow, which is the only path
/// that writes checkpoints at all.
fn growable_image() -> NamedTempFile {
    let image = create_fat32_image(128);
    extend_image(image.path(), 512);
    image
}

/// A filesystem whose FAT already has room for the grown volume.
///
/// `mkfs.fat` sizes the FAT tighter than the formula the resize uses, so a
/// freshly made image always takes the FAT-growth path. 128 MiB grown to
/// 129 MiB (264192 sectors) by this tool has a FAT of 2048 sectors, which
/// still covers 264224 sectors, so growing by those 32 sectors does not grow
/// the FAT. The same fixture lives in `integration_test.rs`.
fn image_ready_for_a_resize_without_fat_growth() -> NamedTempFile {
    let image = create_fat32_image(128);
    extend_image(image.path(), 129);
    resize(image.path());
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(image.path())
        .expect("Failed to open image");
    file.set_len(264224 * 512).expect("Failed to extend image");
    image
}

fn resize(path: &Path) {
    let options = ResizeOptions::new(path).dry_run(false).verbose(false);
    resize_fat32(options).expect("resize failed");
}

/// [F-R17]'s ground: the FAT extension must be fully written before the phase-2
/// checkpoint records that it was, because recovery reads that checkpoint and
/// skips the FAT work on the strength of it.
///
/// This asserts the *ordering*, which holds today. What does not hold today is
/// that the FAT sectors are flushed before the checkpoint is -- that is the fix,
/// and it may not disturb this sequence.
#[test]
#[ignore] // Requires mkfs.fat
fn a_fat_growth_resize_writes_the_fat_extension_before_the_phase_2_checkpoint() {
    let image = growable_image();

    journal::reset();
    resize(image.path());
    let ops = journal::recorded();

    let l = layout_of(image.path());

    // The commit phase opens with the boot-sector restore, and is the last time
    // sector 0 is written, so it bounds the danger zone from above.
    let commit_boot = ops
        .iter()
        .rposition(|op| is_write_to(op, 0))
        .expect("the resize must write the boot sector");

    let last_fat_write = ops[..commit_boot]
        .iter()
        .rposition(|op| is_write_within(op, l.fat_start, l.fat_end))
        .expect("a FAT-growth resize must write into the FAT region");

    // Searching only the window after the last FAT write is what makes this an
    // ordering assertion: a checkpoint written before the FAT extension was
    // finished would leave this window empty.
    ops[last_fat_write + 1..commit_boot]
        .iter()
        .position(|op| is_write_to(op, l.checkpoint))
        .expect(
            "the phase-2 checkpoint must be written after the FAT extension \
             and before the commit phase",
        );

    assert!(
        check_filesystem(image.path()),
        "dosfsck rejected the result"
    );
}

/// [F-R17]: ordering the writes is not enough. The phase-2 checkpoint is a
/// promise that the FAT extension is on disk, and recovery skips the FAT work on
/// the strength of it. Without a barrier the device is free to commit the
/// checkpoint first, and a power loss there leaves a filesystem whose recovery
/// refuses to redo work that never landed.
#[test]
#[ignore] // Requires mkfs.fat
fn the_fat_extension_is_flushed_before_the_phase_2_checkpoint() {
    let image = growable_image();

    journal::reset();
    resize(image.path());
    let ops = journal::recorded();

    let l = layout_of(image.path());

    let commit_boot = ops
        .iter()
        .rposition(|op| is_write_to(op, 0))
        .expect("the resize must write the boot sector");

    let last_fat_write = ops[..commit_boot]
        .iter()
        .rposition(|op| is_write_within(op, l.fat_start, l.fat_end))
        .expect("a FAT-growth resize must write into the FAT region");

    let phase2_checkpoint = last_fat_write
        + 1
        + ops[last_fat_write + 1..commit_boot]
            .iter()
            .position(|op| is_write_to(op, l.checkpoint))
            .expect("the phase-2 checkpoint must follow the FAT extension");

    assert!(
        ops[last_fat_write + 1..phase2_checkpoint].contains(&Op::Sync),
        "the FAT extension must be flushed before the checkpoint that claims \
         it is durable; ops between them were {:?}",
        &ops[last_fat_write + 1..phase2_checkpoint]
    );
}

/// [F-R18]'s ground: the commit phase publishes the new geometry and then erases
/// the checkpoint. That order must hold -- erasing first would leave a device
/// with an invalidated signature and no recovery record, which the tool's own
/// `check_for_incomplete_resize` treats as unrecoverable.
#[test]
#[ignore] // Requires mkfs.fat
fn the_commit_phase_writes_boot_backup_and_fsinfo_before_clearing_the_checkpoint() {
    let image = growable_image();

    journal::reset();
    resize(image.path());
    let ops = journal::recorded();

    let l = layout_of(image.path());

    // Indices are taken over the whole journal rather than a window opening at
    // the primary boot-sector write. [F-R26] made that write the *last* step of
    // the commit phase instead of the first, so slicing there would exclude the
    // very writes this test is about. The assertions below are unchanged.
    let backup = ops
        .iter()
        .rposition(|op| is_write_to(op, l.backup))
        .expect("the commit phase must write the backup boot sector");
    let fsinfo = ops
        .iter()
        .rposition(|op| is_write_to(op, l.fs_info))
        .expect("the commit phase must write FSInfo");
    let clear = ops
        .iter()
        .rposition(|op| is_write_to(op, l.checkpoint))
        .expect("the commit phase must clear the checkpoint");

    assert!(
        backup < clear,
        "backup boot sector must be written before the checkpoint is cleared"
    );
    assert!(
        fsinfo < clear,
        "FSInfo must be written before the checkpoint is cleared"
    );
    assert_eq!(
        ops.last(),
        Some(&Op::Sync),
        "a resize must end with a flush, so nothing it published is left in cache"
    );
}

/// [F-R18]: clearing the checkpoint retires the only record that can recover
/// this filesystem, so everything it was protecting must be durable first.
/// Unsynced, the erase may reach the platter before the restored boot sector,
/// and a power loss there leaves an invalidated signature with no checkpoint --
/// the one combination `check_for_incomplete_resize` reports as
/// `InvalidatedFilesystem` and refuses to touch.
#[test]
#[ignore] // Requires mkfs.fat
fn the_new_geometry_is_flushed_before_the_checkpoint_is_cleared() {
    let image = growable_image();

    journal::reset();
    resize(image.path());
    let ops = journal::recorded();

    let l = layout_of(image.path());

    let clear = ops
        .iter()
        .rposition(|op| is_write_to(op, l.checkpoint))
        .expect("the commit phase must clear the checkpoint");

    let fsinfo = ops[..clear]
        .iter()
        .rposition(|op| is_write_to(op, l.fs_info))
        .expect("the commit phase must write FSInfo before clearing");

    assert!(
        ops[fsinfo + 1..clear].contains(&Op::Sync),
        "the restored boot sector, backup and FSInfo must be flushed before the \
         checkpoint protecting them is erased; ops between them were {:?}",
        &ops[fsinfo + 1..clear]
    );
}

/// [F-R26]: the primary boot sector is the single commit point for the resize,
/// so everything the new geometry depends on must be durable before it lands.
/// Writing the primary first opened a window where a valid new-geometry primary
/// sat against a stale old-geometry backup -- reported as `BackupMismatch` and
/// refused. [F-R4] has since given that refusal an escape
/// (`ResizeOptions::ignore_backup_mismatch`), but it asks the caller to vouch
/// for the primary, so this ordering must still not manufacture the state.
#[test]
#[ignore] // Requires mkfs.fat
fn the_backup_and_fsinfo_are_durable_before_the_primary_boot_sector_commits() {
    let image = growable_image();

    journal::reset();
    resize(image.path());
    let ops = journal::recorded();

    let l = layout_of(image.path());

    let commit = ops
        .iter()
        .rposition(|op| is_write_to(op, 0))
        .expect("the resize must write the primary boot sector");

    let backup = ops[..commit]
        .iter()
        .rposition(|op| is_write_to(op, l.backup))
        .expect("the backup boot sector must be written before the commit point");
    let fsinfo = ops[..commit]
        .iter()
        .rposition(|op| is_write_to(op, l.fs_info))
        .expect("FSInfo must be written before the commit point");

    let last_support = backup.max(fsinfo);
    assert!(
        ops[last_support + 1..commit].contains(&Op::Sync),
        "backup and FSInfo must be flushed before the primary boot sector \
         publishes a geometry that assumes them; ops between them were {:?}",
        &ops[last_support + 1..commit]
    );
}

/// [F-R26] and [F-R56] both move what the last sector holds. Whatever they
/// change, a completed resize may never leave a record that a later run would
/// read as an interrupted one.
#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn no_valid_checkpoint_survives_a_fat_growth_resize() {
    let image = growable_image();
    resize(image.path());

    let l = layout_of(image.path());
    let last = read_sector(image.path(), l.checkpoint, l.sector_size);

    assert_ne!(
        &last[..8],
        &CHECKPOINT_MAGIC[..],
        "a completed resize must not leave a checkpoint record behind"
    );
    assert!(
        check_filesystem(image.path()),
        "dosfsck rejected the result"
    );
}

/// The same invariant on the path that writes no checkpoint at all today. This
/// is the path [F-R26] brings under checkpoint protection and [F-R56] stops
/// blindly zeroing, so it is the one most likely to end up holding a stale
/// record by accident.
#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn no_valid_checkpoint_survives_a_resize_without_fat_growth() {
    // [F-12]: 128 -> 150 MB, which this test used before, grows the FAT.
    let image = image_ready_for_a_resize_without_fat_growth();
    let result =
        resize_fat32(ResizeOptions::new(image.path()).dry_run(false)).expect("resize failed");
    assert!(
        !result.fat_grew,
        "fixture drifted: this case must not grow the FAT"
    );

    let l = layout_of(image.path());
    let last = read_sector(image.path(), l.checkpoint, l.sector_size);

    assert_ne!(
        &last[..8],
        &CHECKPOINT_MAGIC[..],
        "a completed no-growth resize must not leave a checkpoint record behind"
    );
    assert!(
        check_filesystem(image.path()),
        "dosfsck rejected the result"
    );
}

/// A resize that grows the FAT still has to survive being run again. The second
/// run must report "already at maximum size" -- the library's way of saying
/// there is nothing to do -- rather than discovering a checkpoint the first run
/// failed to erase and trying to recover from it.
///
/// The distinction is the whole point: both outcomes are `Err`, but only one of
/// them means the device was left in a state a later run misreads.
#[test]
#[ignore] // Requires mkfs.fat and dosfsck
fn a_second_resize_after_a_completed_one_finds_nothing_to_recover() {
    let image = growable_image();
    resize(image.path());

    let options = ResizeOptions::new(image.path())
        .dry_run(false)
        .verbose(false);
    match resize_fat32(options) {
        Err(Error::AlreadyMaxSize) => {}
        other => panic!("second resize must report AlreadyMaxSize, got {:?}", other),
    }

    assert!(
        check_filesystem(image.path()),
        "dosfsck rejected the result"
    );
}
