use thiserror::Error;

/// All errors that can occur during FAT32 resize operations
#[derive(Debug, Error)]
pub enum Error {
    #[error("Device '{0}' not found or cannot be opened")]
    DeviceNotFound(String),

    #[error("Device '{0}' is currently mounted at '{1}'")]
    DeviceMounted(String, String),

    #[error("Not a valid FAT32 filesystem: {0}")]
    InvalidFAT32(String),

    #[error("Boot sector validation failed: {0}")]
    BootSectorValidation(String),

    #[error("FSInfo sector validation failed: {0}")]
    FSInfoValidation(String),

    #[error("Backup boot sector does not match primary boot sector")]
    BackupMismatch,

    #[error(
        "Device is smaller than current filesystem ({current} sectors < {minimum} sectors needed)"
    )]
    DeviceTooSmall { current: u64, minimum: u64 },

    #[error("Filesystem is already at maximum size for this device")]
    AlreadyMaxSize,

    #[error("Cannot shrink filesystem (not supported)")]
    ShrinkNotSupported,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Calculation error: {0}")]
    Calculation(String),

    #[error("Relocation failed: {0}")]
    Relocation(String),

    #[error("Verification failed: {0}")]
    Verification(String),

    #[error("Cluster {0} is in use and would be overwritten by FAT growth")]
    ClusterInUse(u32),

    #[error("Failed to find free cluster for relocation")]
    NoFreeCluster,

    #[error("Device sector size {0} is not supported (expected 512 or 4096)")]
    UnsupportedSectorSize(u32),

    #[error("FAT table entry is corrupted at cluster {0}")]
    CorruptedFAT(u32),

    #[error("Resize checkpoint is corrupted (CRC mismatch)")]
    CheckpointCorrupted,

    #[error("Filesystem has been invalidated by an interrupted resize operation. Checkpoint not found or corrupted - cannot recover automatically.")]
    InvalidatedFilesystem,

    #[error(
        "Incomplete resize detected at phase {0}, but device size changed. Cannot safely resume."
    )]
    ResizeSizeMismatch(u8),
}

pub type Result<T> = std::result::Result<T, Error>;
