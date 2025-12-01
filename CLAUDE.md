# CLAUDE.md - Project Guide for fat32expander

## Project Overview

`fat32expander` is a Rust CLI tool that enlarges FAT32 filesystems in-place after their underlying partition has been grown (e.g., with `parted`). It handles the complex case where FAT tables need to grow, which requires relocating data clusters.

## Quick Commands

```bash
# Build
cargo build --release

# Run tests (requires dosfstools and mtools)
./scripts/test-resize.sh

# Run unit tests
cargo test

# Run integration tests (requires mkfs.fat, dosfsck)
cargo test --test integration_test -- --ignored
```

## Architecture

```
src/
├── main.rs              # CLI entry point (clap-based)
├── lib.rs               # Library exports
├── error.rs             # Error types (thiserror)
├── device.rs            # Sector-based device I/O wrapper
├── system.rs            # Mount checking via /proc/mounts
├── fat32/
│   ├── mod.rs           # Module exports
│   ├── structs.rs       # BootSector, FSInfo with byte-level accessors
│   ├── validation.rs    # Boot sector and FSInfo validation
│   └── operations.rs    # FAT table read/write, cluster operations
└── resize/
    ├── mod.rs           # Module exports
    ├── calculator.rs    # Size calculations for resize
    ├── relocator.rs     # Cluster relocation logic (critical!)
    └── executor.rs      # Main resize orchestration
```

## Key Concepts

### FAT32 Layout
```
[Boot Sector][FSInfo][Reserved...][FAT1][FAT2][Data Clusters...]
     ^          ^         ^          ^     ^        ^
   Sector 0   Sector 1   ...      First  Copy   Cluster 2 starts here
                                   FAT
```

- **First data sector** = `reserved_sectors + (num_fats * fat_size)`
- **Cluster numbering** starts at 2 (clusters 0-1 are reserved)
- **Cluster to sector**: `first_data_sector + (cluster - 2) * sectors_per_cluster`

### The Relocation Problem

When resizing causes FAT tables to grow, the first data sector moves forward. Clusters that occupied the space where the expanded FAT will be written must be relocated:

```
Before:  [Reserved][FAT1][FAT2][Cluster2][Cluster3][Cluster4]...
After:   [Reserved][FAT1 (bigger)][FAT2 (bigger)][Cluster2'][Cluster3']...
```

**Critical insight**: When copying cluster data during relocation:
- **Source sector** uses OLD `first_data_sector` (data is still in old location)
- **Destination sector** uses NEW `first_data_sector` (write to new layout)

This is implemented in `src/resize/relocator.rs:execute_relocation()`.

### Directory Entry Updates

FAT32 directory entries contain cluster pointers at:
- Bytes 20-21: High 16 bits of starting cluster
- Bytes 26-27: Low 16 bits of starting cluster

When clusters are relocated, ALL directory entries pointing to moved clusters must be updated. This includes entries in subdirectories (which are themselves stored in clusters).

## Common Pitfalls

1. **Sector calculation mismatch**: Always be clear whether you're calculating sectors in the OLD layout or NEW layout when relocating data.

2. **FAT table sync**: Both FAT1 and FAT2 must be updated identically.

3. **Root cluster**: The root directory is a cluster chain starting at `boot.root_cluster()`. If cluster 2 is relocated, the boot sector's root cluster field must be updated.

4. **FSInfo free count**: Must be updated to reflect new available clusters.

## Testing

The test script (`scripts/test-resize.sh`) creates FAT32 images using `mkfs.fat`, adds files with `mtools`, resizes, and verifies:
1. Filesystem integrity with `dosfsck`
2. File presence with `mdir`
3. File content with `mcopy`

Key test scenarios:
- Empty filesystem resize
- Resize without FAT growth (small increase)
- Resize with FAT growth (requires relocation)
- Files in root directory
- Nested subdirectories
- Deep directory hierarchies

## Dependencies

- **Runtime**: Linux only (uses `/proc/mounts` for mount checking)
- **Build**: Rust
- **Testing**: `dosfstools` (mkfs.fat, dosfsck), `mtools` (mcopy, mdir, mmd)

## Error Handling

All errors use `thiserror` and propagate via `Result<T, Error>`. The tool refuses to operate on:
- Mounted filesystems
- Non-FAT32 filesystems
- Filesystems already at maximum size
- Filesystems with mismatched backup boot sectors
