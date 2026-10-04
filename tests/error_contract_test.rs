//! Safety-net tests for the CLI's error contract.
//!
//! These pin behaviour that must survive the [F-R5], [F-R29], [F-R30], [F-R31]
//! and [F-R33] fixes. They deliberately assert on *observable* CLI behaviour --
//! which failures enter crash recovery, and that every failure mode is
//! distinguishable and non-zero -- rather than on the mechanism, because the
//! mechanism is exactly what those fixes replace.
//!
//! No `mkfs.fat` is needed: the CLI's recovery decision is taken on the error
//! returned by `get_fs_info`, which a hand-built image reaches directly.

use std::process::{Command, Output};
use tempfile::NamedTempFile;

const BIN: &str = env!("CARGO_BIN_EXE_fat32expander");

/// An image whose boot sector is a plausible FAT32 layout except that the boot
/// signature is set to `signature`.
///
/// The boot sector must be otherwise well-formed: `read_boot_sector` checks the
/// declared sector size before validation ever inspects the signature, so a
/// zeroed image fails for the wrong reason and never reaches the CLI's recovery
/// decision at all.
fn image_with_signature(size_mb: u64, signature: u16) -> NamedTempFile {
    let file = NamedTempFile::new().expect("temp file");
    let mut data = vec![0u8; (size_mb * 1024 * 1024) as usize];

    let total_sectors: u32 = (size_mb * 1024 * 1024 / 512) as u32;
    data[0] = 0xEB;
    data[1] = 0x58;
    data[2] = 0x90;
    data[3..11].copy_from_slice(b"MSDOS5.0");
    data[11..13].copy_from_slice(&512u16.to_le_bytes()); // bytes per sector
    data[13] = 1; // sectors per cluster
    data[14..16].copy_from_slice(&32u16.to_le_bytes()); // reserved sectors
    data[16] = 2; // number of FATs
    data[21] = 0xF8; // media descriptor
    data[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    data[36..40].copy_from_slice(&64u32.to_le_bytes()); // FAT size in sectors
    data[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    data[48] = 0x01; // FSInfo sector
    data[50] = 0x06; // backup boot sector
    data[82..90].copy_from_slice(b"FAT32   ");
    data[510] = (signature & 0xFF) as u8;
    data[511] = (signature >> 8) as u8;

    std::fs::write(file.path(), &data).expect("write image");
    file
}

/// A healthy FAT32 image: `fs_mb` megabytes of filesystem inside a `file_mb`
/// megabyte file, so `file_mb > fs_mb` yields a filesystem the tool can grow.
///
/// Built by hand for the same reason as `image_with_signature` -- no `mkfs.fat`
/// dependency, so these run on every `cargo test` rather than only under
/// `--ignored`. The geometry is chosen to clear the FAT32 minimum of 65525
/// clusters; below that the tool correctly rejects the image as FAT12/16.
fn valid_image(file_mb: u64, fs_mb: u64) -> NamedTempFile {
    let file = NamedTempFile::new().expect("temp file");
    let mut data = vec![0u8; (file_mb * 1024 * 1024) as usize];

    let total_sectors: u32 = (fs_mb * 1024 * 1024 / 512) as u32;
    data[0] = 0xEB;
    data[1] = 0x58;
    data[2] = 0x90;
    data[3..11].copy_from_slice(b"MSDOS5.0");
    data[11..13].copy_from_slice(&512u16.to_le_bytes());
    data[13] = 1; // sectors per cluster
    data[14..16].copy_from_slice(&32u16.to_le_bytes()); // reserved sectors
    data[16] = 2; // number of FATs
    data[21] = 0xF8; // media descriptor
    data[32..36].copy_from_slice(&total_sectors.to_le_bytes());
    data[36..40].copy_from_slice(&1024u32.to_le_bytes()); // FAT size in sectors
    data[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    data[48] = 0x01; // FSInfo sector
    data[50] = 0x06; // backup boot sector
    data[82..90].copy_from_slice(b"FAT32   ");
    data[510] = 0x55;
    data[511] = 0xAA;

    // FSInfo at sector 1, with both counts set to the "unknown" sentinel.
    let o = 512;
    data[o..o + 4].copy_from_slice(b"RRaA");
    data[o + 484..o + 488].copy_from_slice(b"rrAa");
    data[o + 488..o + 492].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    data[o + 492..o + 496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    data[o + 508..o + 512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

    // Backup boot sector at sector 6 must match the primary.
    let primary = data[0..512].to_vec();
    data[6 * 512..7 * 512].copy_from_slice(&primary);

    std::fs::write(file.path(), &data).expect("write image");
    file
}

fn run(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(BIN)
        .args(args)
        .output()
        .expect("failed to run fat32expander")
}

/// Run `resize` against `path`, with no override flags at all.
///
/// This used to pass `--force`, purely to get past the `geteuid() == 0` gate
/// that [F-R46] removed -- the test suite does not run as root, and a file
/// image it owns needs no privileges to rewrite. The kernel decides write
/// access; there is nothing left here to bypass.
fn run_resize(path: &std::path::Path) -> Output {
    Command::new(BIN)
        .arg("resize")
        .arg(path)
        .output()
        .expect("failed to run fat32expander")
}

/// Run `resize` against `path`, opting in to overriding a backup mismatch.
fn run_resize_ignoring_backup_mismatch(path: &std::path::Path) -> Output {
    Command::new(BIN)
        .arg("resize")
        .arg(path)
        .arg("--ignore-backup-mismatch")
        .output()
        .expect("failed to run fat32expander")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A growable image whose backup boot sector disagrees with the primary.
///
/// Sector 6 is a byte-for-byte copy of the primary in `valid_image`; changing
/// the total-sector count there is enough for `boot_sectors_match` to reject
/// the pair, and leaves everything else about the image healthy so the run
/// reaches the mismatch check rather than failing validation first.
fn image_with_mismatched_backup(file_mb: u64, fs_mb: u64) -> NamedTempFile {
    let file = valid_image(file_mb, fs_mb);
    let mut data = std::fs::read(file.path()).expect("read image");

    let backup_total_sectors_at = 6 * 512 + 32;
    let divergent: u32 = (fs_mb * 1024 * 1024 / 512) as u32 / 2;
    data[backup_total_sectors_at..backup_total_sectors_at + 4]
        .copy_from_slice(&divergent.to_le_bytes());

    std::fs::write(file.path(), &data).expect("write image");
    file
}

/// A backup boot sector that disagrees with the primary must stop the resize,
/// and must say what stopped it.
///
/// [F-R4]/[F-R46]: `--force` has stopped being a general-purpose bypass. What
/// did not change is that a mismatch is refused *unless the user opts in
/// specifically* -- an override that ended up defaulting to on would trade a
/// false promise for silent data loss, since overriding rewrites the backup
/// from a primary that the mismatch says might itself be the corrupt copy.
///
/// The assertions here are the contract; only the argv `run_resize` builds
/// changed when the flag was renamed.
#[test]
fn a_backup_mismatch_is_refused_and_says_so() {
    let image = image_with_mismatched_backup(96, 64);
    let before = std::fs::read(image.path()).expect("read image");

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        stderr_of(&output),
        String::from_utf8_lossy(&output.stdout)
    );

    assert!(
        !output.status.success(),
        "a backup mismatch must not be resized through by default.\noutput:\n{combined}"
    );
    assert!(
        combined.to_lowercase().contains("backup"),
        "the refusal must name the backup boot sector as the reason.\noutput:\n{combined}"
    );

    let after = std::fs::read(image.path()).expect("read image");
    assert!(
        before == after,
        "a refused resize must not modify the image by a single byte"
    );
}

/// Safety net for [F-05]: the refusal has its own exit code, and it tells the
/// user which flag overrides it.
#[test]
fn a_backup_mismatch_exits_8_and_names_the_override() {
    let image = image_with_mismatched_backup(96, 64);

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        stderr_of(&output),
        String::from_utf8_lossy(&output.stdout)
    );

    assert_eq!(
        output.status.code(),
        Some(8),
        "a backup mismatch must exit 8.\noutput:\n{combined}"
    );
    assert!(
        combined.contains("--ignore-backup-mismatch"),
        "the refusal must name the override flag.\noutput:\n{combined}"
    );
}

/// Safety net for [F-05]: before it changes anything, a resize tells the user
/// how big the filesystem is and how big it will be, in bytes.
#[test]
fn a_dry_run_reports_the_current_and_the_new_size() {
    let image = valid_image(96, 64);

    let output = run(&[
        std::ffi::OsStr::new("resize"),
        std::ffi::OsStr::new("--dry-run"),
        image.path().as_os_str(),
    ]);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr:\n{}",
        stderr_of(&output)
    );
    let current = (64u64 * 1024 * 1024).to_string();
    let new = (96u64 * 1024 * 1024).to_string();
    assert!(
        stdout.contains(&current),
        "the current size ({current} bytes) must be reported.\nstdout:\n{stdout}"
    );
    assert!(
        stdout.contains(&new),
        "the new size ({new} bytes) must be reported.\nstdout:\n{stdout}"
    );
}

/// [F-05]: a device smaller than the filesystem on it is a truncated image or
/// the wrong device. The CLI used to read that as "already at maximum size"
/// and exit 0; the documented code is 7.
#[test]
fn a_device_smaller_than_its_filesystem_exits_7() {
    let image = valid_image(64, 96);
    let before = std::fs::read(image.path()).expect("read image");

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        stderr_of(&output),
        String::from_utf8_lossy(&output.stdout)
    );

    assert_eq!(
        output.status.code(),
        Some(7),
        "a device smaller than its filesystem must exit 7.\noutput:\n{combined}"
    );
    assert!(
        !combined.contains("Nothing to do"),
        "a truncated device is not a no-op.\noutput:\n{combined}"
    );
    let after = std::fs::read(image.path()).expect("read image");
    assert!(
        before == after,
        "a refused resize must not modify the image"
    );
}

/// Write a version-1 checkpoint record into the last sector of `path`.
///
/// Built by hand from the documented layout (TECHGUIDE, `checkpoint.rs`), so
/// the test needs no access to the crate's internals.
fn plant_checkpoint(path: &std::path::Path, phase: u8, geometry: [u32; 4]) {
    let mut data = std::fs::read(path).expect("read image");
    let at = data.len() - 512;
    let mut record = [0u8; 512];
    record[0..8].copy_from_slice(b"FAT32RSZ");
    record[8] = 1; // version
    record[9] = phase;
    for (i, value) in geometry.iter().enumerate() {
        record[12 + 4 * i..16 + 4 * i].copy_from_slice(&value.to_le_bytes());
    }
    let crc = crc32fast::hash(&record[0..28]);
    record[28..32].copy_from_slice(&crc.to_le_bytes());
    data[at..].copy_from_slice(&record);
    std::fs::write(path, &data).expect("write image");
}

/// [F-02]: a checkpoint whose old geometry is not the geometry in the boot
/// sector next to it was not written by an interrupted resize of this
/// filesystem. Resuming from it would commit a geometry computed for another
/// volume, so it must be refused as needing recovery, without a single write.
#[test]
fn a_checkpoint_that_disagrees_with_the_boot_sector_is_refused() {
    let image = valid_image(96, 64);
    let fs_sectors = 64 * 1024 * 1024 / 512;
    let device_sectors = 96 * 1024 * 1024 / 512;
    // FatWritten (2) from a filesystem 1 MiB smaller, with the same FAT size.
    plant_checkpoint(
        image.path(),
        2,
        [fs_sectors - 2048, device_sectors, 1024, 1024],
    );
    let before = std::fs::read(image.path()).expect("read image");

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        stderr_of(&output),
        String::from_utf8_lossy(&output.stdout)
    );

    assert_eq!(
        output.status.code(),
        Some(9),
        "a mismatched checkpoint must exit 9.\noutput:\n{combined}"
    );
    assert!(
        combined.to_lowercase().contains("checkpoint"),
        "the refusal must name the checkpoint.\noutput:\n{combined}"
    );
    let after = std::fs::read(image.path()).expect("read image");
    assert!(
        before == after,
        "a refused resume must not modify the image by a single byte"
    );
}

/// [F-R4]: the override must actually reach the library.
///
/// The CLI used to print "Use --force to proceed anyway" and then hand
/// `resize_fat32` an options struct with no override field in it, so the
/// library refused a second time and the advertised escape was a dead end.
/// Opting in must now get past the mismatch.
#[test]
fn the_backup_mismatch_override_actually_proceeds() {
    let image = image_with_mismatched_backup(96, 64);

    let output = run_resize_ignoring_backup_mismatch(image.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr_of(&output)
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "opting in to the mismatch override must complete the resize.\noutput:\n{combined}"
    );

    let after = std::fs::read(image.path()).expect("read image");
    let total_sectors = u32::from_le_bytes(after[32..36].try_into().unwrap());
    assert!(
        total_sectors as u64 > 64 * 1024 * 1024 / 512,
        "the override must produce a real resize, not just a silent pass, got {total_sectors}"
    );

    // The rewritten backup must now agree with the primary -- the override
    // exists to repair the disagreement, not to leave it in place.
    let backup_total_sectors =
        u32::from_le_bytes(after[6 * 512 + 32..6 * 512 + 36].try_into().unwrap());
    assert_eq!(
        backup_total_sectors, total_sectors,
        "after a forced resize the backup must match the primary again"
    );
}

/// A device the caller cannot write must fail with a message that names it.
///
/// [F-R46] removes the `geteuid() == 0` gate in favour of letting the open
/// fail, on the grounds that the kernel is the authority on write access. That
/// only holds if the resulting diagnostic is still usable: the failure has to
/// name the device, or a script driving several devices cannot tell which one
/// refused.
#[test]
fn an_unwritable_device_fails_and_names_the_path() {
    use std::os::unix::fs::PermissionsExt;

    // Permission bits do not apply to root, so under root this asserts
    // nothing. Skip loudly rather than pass hollowly.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipping: running as root, permission bits do not apply");
        return;
    }

    let image = valid_image(96, 64);
    std::fs::set_permissions(image.path(), std::fs::Permissions::from_mode(0o444))
        .expect("set read-only");

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        stderr_of(&output),
        String::from_utf8_lossy(&output.stdout)
    );
    let path = image.path().display().to_string();

    assert!(
        !output.status.success(),
        "a device that cannot be written must not report success.\noutput:\n{combined}"
    );
    assert!(
        combined.contains(&path),
        "the failure must name the device {path} it refers to.\noutput:\n{combined}"
    );

    // Let the tempfile clean itself up.
    let _ = std::fs::set_permissions(image.path(), std::fs::Permissions::from_mode(0o644));
}

/// The success path through the binary: a growable filesystem must actually be
/// resized, and report it.
///
/// Nothing else covers this. `integration_test.rs` drives the library directly
/// and never observes an exit code, and every other test in this file asserts a
/// failure, a no-op, or a dry run -- so removing the privilege gate could break
/// the one path users actually run and the suite would stay green.
#[test]
fn a_cli_resize_of_a_growable_filesystem_succeeds_and_grows_it() {
    let image = valid_image(96, 64);

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr_of(&output)
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "resizing a growable filesystem must exit 0.\noutput:\n{combined}"
    );

    // The filesystem must now claim the whole 96 MB file rather than its
    // original 64 MB, read back off the device rather than from the report.
    let after = std::fs::read(image.path()).expect("read image");
    let total_sectors = u32::from_le_bytes(after[32..36].try_into().unwrap());
    assert!(
        total_sectors as u64 > 64 * 1024 * 1024 / 512,
        "the resized filesystem must declare more sectors than it started with, got {total_sectors}"
    );
}

/// A zeroed boot signature is how this tool marks a filesystem mid-resize, so
/// the CLI must treat it as a possible interrupted resize and enter recovery.
///
/// This is the behaviour [F-R5] currently achieves by substring-matching a
/// formatted error message. The fix replaces the mechanism; this outcome must
/// not change, or users of a genuinely crashed filesystem lose their recovery
/// path silently.
#[test]
fn invalidated_boot_signature_makes_the_cli_attempt_recovery() {
    let image = image_with_signature(8, 0x0000);
    let output = run_resize(image.path());
    let stderr = stderr_of(&output);

    assert!(
        stderr.contains("Attempting recovery"),
        "an invalidated (0x0000) boot signature must enter crash recovery.\n\
         stderr was:\n{stderr}"
    );
    assert!(
        !output.status.success(),
        "a zeroed image has no recoverable filesystem, so the run must still fail"
    );
}

/// A boot signature that is neither valid nor the tool's own invalidation
/// marker is a corrupt or non-FAT32 device, not an interrupted resize.
/// Entering recovery here would drive the resize machinery over a filesystem
/// this tool never touched.
#[test]
fn garbage_boot_signature_does_not_make_the_cli_attempt_recovery() {
    let image = image_with_signature(8, 0x1234);
    let output = run_resize(image.path());
    let stderr = stderr_of(&output);

    assert!(
        !stderr.contains("Attempting recovery"),
        "a garbage (0x1234) boot signature must NOT be mistaken for an \
         interrupted resize.\nstderr was:\n{stderr}"
    );
    assert!(
        stderr.contains("Failed to read filesystem info"),
        "the failure must be reported as an unreadable filesystem.\n\
         stderr was:\n{stderr}"
    );
    assert!(!output.status.success(), "a garbage image must fail");
}

/// The recovery decision must depend on the state of the device, not on the
/// device being absent. A missing path is a plain open failure.
#[test]
fn a_missing_device_does_not_make_the_cli_attempt_recovery() {
    let output = run_resize(std::path::Path::new(
        "/nonexistent/fat32expander/safety/net/device.img",
    ));
    let stderr = stderr_of(&output);

    assert!(
        !stderr.contains("Attempting recovery"),
        "a missing device must not enter crash recovery.\nstderr was:\n{stderr}"
    );
    assert!(!output.status.success(), "a missing device must fail");
}

/// Every failure mode must remain non-zero and carry a message that tells the
/// modes apart. [F-R31] will give them distinct exit *codes*; that must not
/// come at the cost of collapsing the messages, and no mode may start
/// succeeding.
#[test]
fn distinct_failure_modes_stay_non_zero_and_distinguishable() {
    let missing = run_resize(std::path::Path::new(
        "/nonexistent/fat32expander/safety/net/device.img",
    ));
    let garbage_image = image_with_signature(8, 0x1234);
    let garbage = run_resize(garbage_image.path());
    let invalidated_image = image_with_signature(8, 0x0000);
    let invalidated = run_resize(invalidated_image.path());

    for (name, output) in [
        ("missing device", &missing),
        ("garbage filesystem", &garbage),
        ("invalidated filesystem", &invalidated),
    ] {
        assert!(
            !output.status.success(),
            "{name} must exit non-zero, got {:?}",
            output.status.code()
        );
    }

    // The three must not be reported identically -- a caller has to be able to
    // tell them apart from the output alone.
    let missing_err = stderr_of(&missing);
    let garbage_err = stderr_of(&garbage);
    let invalidated_err = stderr_of(&invalidated);

    assert_ne!(
        missing_err, garbage_err,
        "a missing device and a corrupt filesystem must not report identically"
    );
    assert_ne!(
        garbage_err, invalidated_err,
        "a corrupt filesystem and an interrupted resize must not report identically"
    );
}

/// The device path must appear in the failure output. [F-R29] and [F-R30] add
/// errno and sector context; the path must survive that change, since it is
/// the only thing identifying *which* device failed when a script drives
/// several.
#[test]
fn failures_name_the_device_they_refer_to() {
    let image = image_with_signature(8, 0x1234);
    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        stderr_of(&output),
        String::from_utf8_lossy(&output.stdout)
    );
    let path = image.path().display().to_string();

    assert!(
        combined.contains(&path),
        "the failing device path {path} must appear in the output.\n\
         output was:\n{combined}"
    );
}

/// [F-R31]: a calling script must be able to branch on *why* the run failed.
/// Before this, every one of these exited 1.
///
/// The codes are grouped by what the caller should do about the failure, not
/// one-per-variant -- the error enum changes shape too often for that to be a
/// stable ABI.
#[test]
fn failure_modes_get_distinct_exit_codes() {
    let missing = run_resize(std::path::Path::new(
        "/nonexistent/fat32expander/exit/code/device.img",
    ));
    assert_eq!(
        missing.status.code(),
        Some(3),
        "a device that cannot be opened must exit 3.\nstderr:\n{}",
        stderr_of(&missing)
    );

    let garbage_image = image_with_signature(8, 0x1234);
    let garbage = run_resize(garbage_image.path());
    assert_eq!(
        garbage.status.code(),
        Some(5),
        "an unusable filesystem must exit 5.\nstderr:\n{}",
        stderr_of(&garbage)
    );

    // A *valid* filesystem whose signature is zeroed is a genuinely interrupted
    // resize: recovery is attempted, finds no checkpoint, and gives up.
    let interrupted_image = valid_image(96, 64);
    let mut data = std::fs::read(interrupted_image.path()).unwrap();
    data[510] = 0x00;
    data[511] = 0x00;
    std::fs::write(interrupted_image.path(), &data).unwrap();

    let interrupted = run_resize(interrupted_image.path());
    assert_eq!(
        interrupted.status.code(),
        Some(9),
        "an interrupted resize that cannot be recovered must exit 9.\nstderr:\n{}",
        stderr_of(&interrupted)
    );

    // The 8 MB fixture is too small to be valid FAT32, so it enters recovery
    // (its signature is the invalidation marker) and then fails validation.
    // That is a filesystem problem, not a recovery problem, and must be told
    // apart from the case above.
    let too_small_image = image_with_signature(8, 0x0000);
    let too_small = run_resize(too_small_image.path());
    assert_eq!(
        too_small.status.code(),
        Some(5),
        "entering recovery and then failing validation must exit 5, not 9.\nstderr:\n{}",
        stderr_of(&too_small)
    );
}

/// A resize that finds nothing to do is not a failure. Running the tool twice
/// must succeed twice, which is what makes it safe to put in a provisioning
/// script that does not track whether it has already run.
///
/// This is a deliberate behaviour change made with the developer's approval:
/// before, "already at maximum size" exited non-zero.
#[test]
fn a_filesystem_already_at_maximum_size_exits_zero() {
    let image = valid_image(64, 64);
    let before = std::fs::read(image.path()).expect("read image");

    let output = run_resize(image.path());
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        stderr_of(&output)
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "a filesystem with no room to grow must exit 0.\noutput:\n{combined}"
    );
    assert!(
        combined.contains("maximum size"),
        "it must still say why there was nothing to do.\noutput:\n{combined}"
    );

    let after = std::fs::read(image.path()).expect("read image");
    assert!(
        before == after,
        "a no-op resize must not modify the image by a single byte"
    );
}

/// Running it a second time must behave identically to the first -- that is
/// what "idempotent" has to mean for a script that reruns it.
#[test]
fn resizing_an_already_maximal_filesystem_twice_succeeds_twice() {
    let image = valid_image(64, 64);

    for attempt in 1..=2 {
        let output = run_resize(image.path());
        assert_eq!(
            output.status.code(),
            Some(0),
            "attempt {attempt} must exit 0.\nstderr:\n{}",
            stderr_of(&output)
        );
    }
}

/// The other half of [F-R31], and the half nothing else covers: giving failures
/// distinct non-zero exit codes means rewriting how `main` returns, and the way
/// that goes wrong is the *success* path picking up a non-zero code.
///
/// Every test that runs the binary today asserts a failure, and
/// `integration_test.rs` calls the library directly and never observes an exit
/// code at all -- so a `resize` that reported failure on a completed resize
/// would pass the entire suite. These three pin the successful commands at 0.
#[test]
fn version_exits_zero() {
    let output = run(&[std::ffi::OsStr::new("version")]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "`version` must exit 0.\nstderr was:\n{}",
        stderr_of(&output)
    );
}

#[test]
fn info_on_a_healthy_filesystem_exits_zero() {
    let image = valid_image(64, 64);
    let output = run(&[std::ffi::OsStr::new("info"), image.path().as_os_str()]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "`info` on a healthy filesystem must exit 0.\nstderr was:\n{}",
        stderr_of(&output)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("FAT32 Filesystem Information"),
        "`info` must actually report, not exit 0 having done nothing"
    );
}

/// A dry run is the success path that also touches the resize machinery, so it
/// exercises far more of `main` than `info` does -- and it must still exit 0.
/// It must also leave the image byte-identical, which is what makes a 0 here
/// mean "previewed successfully" rather than "wrote something and survived".
#[test]
fn a_dry_run_on_a_growable_filesystem_exits_zero_and_changes_nothing() {
    let image = valid_image(96, 64);
    let before = std::fs::read(image.path()).expect("read image");

    let output = run(&[
        std::ffi::OsStr::new("resize"),
        std::ffi::OsStr::new("--dry-run"),
        image.path().as_os_str(),
    ]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "a dry-run resize of a growable filesystem must exit 0.\nstderr was:\n{}",
        stderr_of(&output)
    );

    let after = std::fs::read(image.path()).expect("read image");
    assert!(
        before == after,
        "a dry run must not modify the image by a single byte"
    );
}
