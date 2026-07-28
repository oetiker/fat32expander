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

/// Run `resize` against `path`. `--force` is required because the test suite
/// does not run as root; it only bypasses the privilege warning.
fn run_resize(path: &std::path::Path) -> Output {
    Command::new(BIN)
        .arg("resize")
        .arg(path)
        .arg("--force")
        .output()
        .expect("failed to run fat32expander")
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
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
