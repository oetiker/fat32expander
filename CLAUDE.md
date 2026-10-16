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
├── main.rs              # CLI entry point: clap definitions, main, dispatch
├── cli/                 # Binary-only modules, declared from main.rs, never lib.rs
│   ├── exit.rs          # Exit codes (an ABI) and the error-to-code table
│   ├── render.rs        # `info` report, resize events and summary
│   ├── resize.rs        # The `resize` subcommand
│   └── version.rs       # `version` text and build-time formatting
├── lib.rs               # Library exports
├── error.rs             # Error types (thiserror)
├── device.rs            # Sector-based device I/O wrapper
├── system.rs            # Mount check and device size, per system
├── fat32/
│   ├── mod.rs           # Module exports
│   ├── structs.rs       # BootSector, FSInfo with byte-level accessors
│   ├── validation.rs    # Boot sector and FSInfo validation
│   └── operations.rs    # FAT table read/write, cluster operations
└── resize/
    ├── mod.rs           # Module exports
    ├── calculator.rs    # Size calculations for resize
    ├── checkpoint.rs    # On-disk checkpoint format and its sector I/O
    ├── events.rs        # ResizeEvent: what a resize reports while it runs
    ├── executor.rs      # Main resize orchestration (phase witnesses)
    ├── fault.rs         # Crash points for the fault-injection feature
    ├── fsinfo.rs        # Read-only filesystem query (FSInfoReport)
    ├── options.rs       # ResizeOptions and ResizeResult
    ├── relocator.rs     # Cluster relocation logic (critical!)
    └── testutil.rs      # Shared fixtures for the unit tests
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

### Cluster Numbers Are Preserved — No Directory Entry Updates

**Relocation moves physical sectors and nothing else.** Cluster *N* is still cluster *N* after the resize; only the sector it lives at changes, because `first_data_sector` moved forward. Everything that names a cluster by number therefore stays correct on its own:

- FAT chains are not rewritten — every entry still points at the same cluster number.
- Directory entries are not rewritten — their cluster pointers (bytes 20-21 high, 26-27 low) still name the right cluster.
- `root_cluster` in the boot sector is not rewritten — the root directory stays cluster 2, at a new physical location.

See `src/resize/relocator.rs` (`/// Since cluster numbers don't change, no FAT chain or directory entry updates are needed!`) and `TECHGUIDE.md`.

> **Historical note.** An earlier design *did* renumber clusters, which required walking every directory tree and rewriting each entry that pointed at a moved cluster. That approach was abandoned in favour of the sector shift above. If you find code or docs implying directory entries must be updated, it is residue from that design, not a requirement — `BootSector::set_root_cluster` was residue of the same kind and was removed in [F-17]. This section itself described the abandoned design until [F-R60].

## Common Pitfalls

1. **Sector calculation mismatch**: Always be clear whether you're calculating sectors in the OLD layout or NEW layout when relocating data.

2. **FAT table sync**: Both FAT1 and FAT2 must be updated identically.

3. **Root cluster**: The root directory is a cluster chain starting at `boot.root_cluster()`. Relocating cluster 2 does **not** change that field — the root stays cluster 2 and only moves physically. Rewriting the `root_cluster` field during a resize is a bug, not a fix; `root_cluster_survives_a_relocating_resize` in `tests/integration_test.rs` guards this.

4. **FSInfo free count**: Must be updated to reflect new available clusters.

## Testing

The test script (`scripts/test-resize.sh`) creates FAT32 images using `mkfs.fat`, adds files with `mtools`, resizes, and verifies:
1. Filesystem integrity with `dosfsck`
2. File presence with `mdir`
3. File content with `mcopy`

Key test scenarios:
- Empty filesystem resize
- Resize without FAT growth (a filesystem this tool already grew, enlarged by 32 sectors; a fresh `mkfs.fat` image always grows the FAT)
- Resize with FAT growth (requires relocation)
- Files in root directory
- Nested subdirectories
- Deep directory hierarchies

## Dependencies

- **Runtime**: Linux, macOS, illumos. The mount table comes from `/proc/mounts`,
  `getfsstat` and `/etc/mnttab`; the disk size from `BLKGETSIZE64`,
  `DKIOCGETBLOCK*` and `DKIOCGMEDIAINFO`. `O_EXCL` locks the device on Linux
  only. `scripts/test-macos.sh` (CI job `macos`) is the only real-device test
  outside Linux; illumos has unit tests only.
- **Build**: Rust
- **Testing**: `dosfstools` (mkfs.fat, dosfsck), `mtools` (mcopy, mdir, mmd)

## Error Handling

The library's errors use `thiserror` and propagate via `Result<T, Error>`; the
binary wraps them with `anyhow` context and maps them to exit codes in
`src/cli/exit.rs`. The tool refuses to operate on:
- Mounted or otherwise busy devices (exit 4)
- Non-FAT32 filesystems (exit 5)
- Filesystems whose backup boot sector disagrees with the primary (exit 8),
  unless `--ignore-backup-mismatch` is given

A filesystem already at maximum size is not an error: the tool prints
"Nothing to do" and exits 0.
