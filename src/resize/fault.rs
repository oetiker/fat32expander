//! Fault injection for crash-recovery testing.
//!
//! Compiled to a no-op unless the `fault-injection` feature is enabled, so
//! production builds carry none of this.

// ===== Fault Injection for Testing =====
//
// Only available when compiled with --features fault-injection
//
// Set FAT32_CRASH_AT to the name of a [`CrashPoint`] to simulate a crash there.
//
// Build: cargo build --release --features fault-injection
// Usage: FAT32_CRASH_AT=after_boot_invalidate fat32expander resize image.img

/// A place in a resize where fault injection can stop the process.
///
/// [F-23]: the names were bare strings at each call site, so a misspelt one
/// compiled and silently never fired. They are variants now; `name()` is the
/// spelling `FAT32_CRASH_AT`, the QEMU script and the crash tests use, and a
/// unit test checks those files against [`CrashPoint::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CrashPoint {
    /// After writing the phase 0 checkpoint, before the data shift.
    AfterCheckpointStart,
    /// After the data shift, before the phase 1 checkpoint.
    AfterDataShift,
    /// After the phase 1 checkpoint, before the boot sector is invalidated.
    AfterCheckpointDataCopied,
    /// After the boot sector is invalidated, before the FAT is written.
    AfterBootInvalidate,
    /// After the FAT is written, before the phase 2 checkpoint.
    AfterFatWrite,
    /// After the phase 2 checkpoint, before the boot sector is restored.
    AfterCheckpointFatWritten,
    /// After the backup boot sector and FSInfo are flushed, before the
    /// primary boot sector commits the new geometry.
    BeforePrimaryBootWrite,
}

impl CrashPoint {
    /// Every crash point, in the order a resize passes them.
    #[cfg(test)]
    pub(crate) const ALL: [CrashPoint; 7] = [
        CrashPoint::AfterCheckpointStart,
        CrashPoint::AfterDataShift,
        CrashPoint::AfterCheckpointDataCopied,
        CrashPoint::AfterBootInvalidate,
        CrashPoint::AfterFatWrite,
        CrashPoint::AfterCheckpointFatWritten,
        CrashPoint::BeforePrimaryBootWrite,
    ];

    /// The name `FAT32_CRASH_AT` selects this point by.
    #[cfg(any(test, feature = "fault-injection"))]
    pub(crate) fn name(self) -> &'static str {
        match self {
            CrashPoint::AfterCheckpointStart => "after_checkpoint_start",
            CrashPoint::AfterDataShift => "after_data_shift",
            CrashPoint::AfterCheckpointDataCopied => "after_checkpoint_data_copied",
            CrashPoint::AfterBootInvalidate => "after_boot_invalidate",
            CrashPoint::AfterFatWrite => "after_fat_write",
            CrashPoint::AfterCheckpointFatWritten => "after_checkpoint_fat_written",
            CrashPoint::BeforePrimaryBootWrite => "before_primary_boot_write",
        }
    }
}

/// Check if we should crash at a specific point (for testing crash recovery)
/// This function only does anything when compiled with the fault-injection feature
#[cfg(feature = "fault-injection")]
pub(crate) fn maybe_crash_at(point: CrashPoint) {
    if let Ok(crash_point) = std::env::var("FAT32_CRASH_AT") {
        if crash_point == point.name() {
            eprintln!("FAULT INJECTION: Simulating crash at '{}'", point.name());
            std::process::exit(137); // Exit like killed by SIGKILL
        }
    }
}

/// No-op version for production builds
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub(crate) fn maybe_crash_at(_point: CrashPoint) {
    // No-op in production builds
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The names are an interface: `FAT32_CRASH_AT`, the QEMU script and the
    /// crash tests all spell them. Renaming one is a breaking change there.
    #[test]
    fn crash_point_names_are_stable() {
        let names: Vec<&str> = CrashPoint::ALL.iter().map(|p| p.name()).collect();
        assert_eq!(
            names,
            [
                "after_checkpoint_start",
                "after_data_shift",
                "after_checkpoint_data_copied",
                "after_boot_invalidate",
                "after_fat_write",
                "after_checkpoint_fat_written",
                "before_primary_boot_write",
            ]
        );
    }

    /// Every word in `text` that looks like a crash-point name.
    fn crash_point_words(text: &str) -> BTreeSet<String> {
        text.split(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
            .filter(|w| w.starts_with("after_") || w.starts_with("before_"))
            .map(str::to_string)
            .collect()
    }

    fn read(relative: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// [F-23]: the files that name crash points as plain strings must name
    /// exactly the points the code has. A misspelt name would select a crash
    /// point that never fires, and the run would pass without crashing.
    #[test]
    fn every_file_naming_crash_points_names_exactly_the_real_ones() {
        let all: BTreeSet<String> = CrashPoint::ALL
            .iter()
            .map(|p| p.name().to_string())
            .collect();

        let script = read("scripts/qemu/test-crash-recovery.sh");
        let list = script
            .lines()
            .find_map(|l| l.trim().strip_prefix("CRASH_POINTS="))
            .expect("CRASH_POINTS= line in the QEMU script");

        for (what, found) in [
            ("CRASH_POINTS in the QEMU script", crash_point_words(list)),
            (
                "scripts/qemu/test-crash-recovery.sh",
                crash_point_words(&script),
            ),
            ("TECHGUIDE.md", crash_point_words(&read("TECHGUIDE.md"))),
            (
                "tests/crash_recovery_test.rs",
                crash_point_words(&read("tests/crash_recovery_test.rs")),
            ),
        ] {
            assert_eq!(found, all, "crash points named in {what}");
        }
    }
}
