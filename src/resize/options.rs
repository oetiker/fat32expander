//! Input and output types for a resize operation.
//!
//! Plain data crossing the library boundary, split out of `executor.rs` for
//! [F-R42] so the algorithm file holds an algorithm.

use crate::resize::calculator::SizeCalculation;

/// Options for the resize operation
#[derive(Debug, Clone)]
pub struct ResizeOptions {
    device_path: std::path::PathBuf,
    dry_run: bool,
    verbose: bool,
    ignore_backup_mismatch: bool,
}

impl ResizeOptions {
    /// Create new resize options for the given device path
    pub fn new(device_path: impl AsRef<std::path::Path>) -> Self {
        Self {
            device_path: device_path.as_ref().to_path_buf(),
            dry_run: false,
            verbose: false,
            ignore_backup_mismatch: false,
        }
    }

    /// Enable or disable dry run mode (don't make changes)
    pub fn dry_run(mut self, enable: bool) -> Self {
        self.dry_run = enable;
        self
    }

    /// Enable or disable verbose output
    pub fn verbose(mut self, enable: bool) -> Self {
        self.verbose = enable;
        self
    }

    /// Get the device path
    pub fn device_path(&self) -> &std::path::Path {
        &self.device_path
    }

    /// Check if dry run mode is enabled
    pub fn is_dry_run(&self) -> bool {
        self.dry_run
    }

    /// Check if verbose mode is enabled
    pub fn is_verbose(&self) -> bool {
        self.verbose
    }

    /// Proceed even though the backup boot sector disagrees with the primary.
    ///
    /// Off by default, and deliberately not a general-purpose "force" ([F-R46]):
    /// it opens exactly one check. A mismatch means the two copies of the
    /// geometry disagree, and the resize resolves that by trusting the primary
    /// and rewriting the backup from it. If the *primary* is the damaged copy,
    /// that destroys the only good record of the original layout -- so this is
    /// an assertion by the caller that the primary is the correct one, not a
    /// way to make a warning go away.
    ///
    /// [F-R4]: the CLI advertised this escape long before the library carried
    /// the field to honour it, so `resize_fat32` refused a second time and the
    /// advertised flag was a dead end.
    pub fn ignore_backup_mismatch(mut self, enable: bool) -> Self {
        self.ignore_backup_mismatch = enable;
        self
    }

    /// Check whether a backup boot sector mismatch is being overridden
    pub fn ignores_backup_mismatch(&self) -> bool {
        self.ignore_backup_mismatch
    }
}

/// Result of a resize operation
#[derive(Debug)]
pub struct ResizeResult {
    /// Old filesystem size in bytes
    pub old_size_bytes: u64,
    /// New filesystem size in bytes
    pub new_size_bytes: u64,
    /// Whether FAT tables grew
    pub fat_grew: bool,
    /// Number of clusters relocated
    pub clusters_relocated: usize,
    /// Detailed calculation results
    pub calculation: SizeCalculation,
    /// List of operations performed (for logging)
    pub operations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resize_options() {
        let opts = ResizeOptions::new("/dev/sda1").dry_run(true).verbose(false);

        assert_eq!(opts.device_path(), std::path::Path::new("/dev/sda1"));
        assert!(opts.is_dry_run());
        assert!(!opts.is_verbose());
    }

    #[test]
    fn test_resize_result() {
        let calc = SizeCalculation {
            old_total_sectors: 1000000,
            new_total_sectors: 2000000,
            old_fat_size: 1000,
            new_fat_size: 2000,
            new_data_clusters: 200000,
            new_free_clusters: 100000,
            fat_needs_growth: true,
            fat_growth_sectors: 1000,
            first_affected_cluster: 2,
            last_affected_cluster: 10,
        };

        let result = ResizeResult {
            old_size_bytes: 512000000,
            new_size_bytes: 1024000000,
            fat_grew: true,
            clusters_relocated: 5,
            calculation: calc,
            operations: vec!["test".to_string()],
        };

        assert_eq!(result.old_size_bytes, 512000000);
        assert!(result.fat_grew);
        assert_eq!(result.clusters_relocated, 5);
    }
}
