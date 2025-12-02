pub mod calculator;
pub mod executor;
pub mod relocator;

// Re-export calculator types and functions
pub use calculator::{calculate_fat_size, calculate_new_size, SizeCalculation};

// Re-export executor types and functions
pub use executor::{
    get_fs_info, resize_fat32, FSInfoReport, ResizeCheckpoint, ResizeOptions, ResizePhase,
    ResizeResult,
};

// Re-export relocator types and functions
pub use relocator::{
    execute_relocation, plan_relocation, verify_relocation, ClusterMove, RelocationPlan,
};
