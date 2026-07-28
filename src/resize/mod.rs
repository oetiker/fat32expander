pub mod calculator;
pub mod checkpoint;
pub mod executor;
mod fault;
pub mod fsinfo;
pub mod options;
pub mod relocator;

#[cfg(test)]
mod testutil;

// Re-export calculator types and functions
pub use calculator::{calculate_fat_size, calculate_new_size, SizeCalculation};

// Re-export the checkpoint record. The functions that apply one stay crate-private:
// an external caller able to build and serialise a checkpoint but never apply one
// is exactly the incoherence [F-R44] flags.
pub use checkpoint::{ResizeCheckpoint, ResizePhase};

// Re-export the resize entry point and its data types
pub use executor::resize_fat32;
pub use fsinfo::{get_fs_info, FSInfoReport};
pub use options::{ResizeOptions, ResizeResult};

// Re-export relocator types and functions
pub use relocator::{
    execute_relocation, plan_relocation, ClusterMove, RelocationPlan, ShiftProgress,
};
