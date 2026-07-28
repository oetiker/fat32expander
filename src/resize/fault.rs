//! Fault injection for crash-recovery testing.
//!
//! Compiled to a no-op unless the `fault-injection` feature is enabled, so
//! production builds carry none of this.

// ===== Fault Injection for Testing =====
//
// Only available when compiled with --features fault-injection
//
// Set FAT32_CRASH_AT environment variable to simulate crashes at specific points:
//   - "after_checkpoint_start" - after writing phase 0 checkpoint, before data shift
//   - "after_data_shift" - after data shift, before phase 1 checkpoint
//   - "after_checkpoint_data_copied" - after phase 1 checkpoint, before boot invalidation
//   - "after_boot_invalidate" - after boot sector invalidated, before FAT operations
//   - "after_fat_write" - after FAT written, before phase 2 checkpoint
//   - "after_checkpoint_fat_written" - after phase 2 checkpoint, before boot restore
//
// Build: cargo build --release --features fault-injection
// Usage: FAT32_CRASH_AT=after_boot_invalidate fat32expander resize image.img

/// Check if we should crash at a specific point (for testing crash recovery)
/// This function only does anything when compiled with the fault-injection feature
#[cfg(feature = "fault-injection")]
pub(crate) fn maybe_crash_at(point: &str) {
    if let Ok(crash_point) = std::env::var("FAT32_CRASH_AT") {
        if crash_point == point {
            eprintln!("FAULT INJECTION: Simulating crash at '{}'", point);
            std::process::exit(137); // Exit like killed by SIGKILL
        }
    }
}

/// No-op version for production builds
#[cfg(not(feature = "fault-injection"))]
#[inline(always)]
pub(crate) fn maybe_crash_at(_point: &str) {
    // No-op in production builds
}
