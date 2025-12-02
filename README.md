# fat32expander

[![Rust Tests](https://github.com/oetiker/fat32expander/actions/workflows/tests.yml/badge.svg)](https://github.com/oetiker/fat32expander/actions/workflows/tests.yml)
[![Latest Release](https://img.shields.io/github/v/release/oetiker/fat32expander)](https://github.com/oetiker/fat32expander/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A command-line tool to expand FAT32 filesystems in-place after their underlying partition or disk image has been grown.

## Warning

**Filesystem modification is inherently risky.** While this tool includes safeguards, data loss is possible due to hardware failure, power loss, bugs, or unexpected conditions. Always maintain backups of important data before modifying any filesystem.

## Overview

When you resize a partition (e.g., with `parted`, `fdisk`, or by expanding a virtual disk), the filesystem inside does not automatically grow to fill the new space. `fat32expander` handles this for FAT32 filesystems, including the case where the FAT tables need to grow, which requires relocating data clusters.

## Features

- **In-place expansion** - Modifies filesystem directly without requiring temporary storage
- **FAT table growth** - Relocates clusters when FAT tables need to expand
- **Multiple sector sizes** - Supports 512, 1024, 2048, and 4096-byte sectors (including EFI partitions on 4Kn drives)
- **Crash recovery** - Resumes interrupted operations; protects against partial completion
- **Dry-run mode** - Preview changes without modifying the filesystem
- **Verbose output** - Detailed logging of all operations

## Installation

### Pre-built Binaries

Download from the [releases page](https://github.com/oetiker/fat32expander/releases).

### Building from Source

Requires Rust 1.70 or later.

```bash
# Clone the repository
git clone https://github.com/oetiker/fat32expander.git
cd fat32expander

# Build release binary
make release

# Or build static Linux binary (recommended for portability)
make static
```

### Cross-compilation

```bash
# Install Rust targets
make setup-targets

# Build for specific platforms
make linux-x64      # Linux x86_64 (static)
make linux-arm64    # Linux ARM64 (static)
make windows        # Windows x86_64
make macos-x64      # macOS Intel
make macos-arm64    # macOS Apple Silicon

# Build distribution package (Linux + Windows)
make dist
```

## Usage

### Basic Usage

```bash
# First, grow the partition (example using parted)
sudo parted /dev/sdX resizepart 1 100%

# Then expand the filesystem
sudo fat32expander resize /dev/sdX1
```

### Commands

```bash
# Show filesystem information
fat32expander info /dev/sdX1

# Expand filesystem to fill available space
fat32expander resize /dev/sdX1

# Preview resize without making changes
fat32expander resize --dry-run /dev/sdX1

# Verbose output
fat32expander resize --verbose /dev/sdX1
```

### Working with Disk Images

```bash
# Expand the image file first
truncate -s 2G disk.img

# Then expand the filesystem
fat32expander resize disk.img
```

## How It Works

### FAT32 Layout

```
[Boot Sector][FSInfo][Reserved...][FAT1][FAT2][Data Clusters...]
```

When a FAT32 filesystem grows significantly, the FAT (File Allocation Table) may need more space to track additional clusters. Since the FAT is located before the data area, expanding it requires:

1. **Shifting data forward** - All cluster data must move to new sector positions
2. **Extending FAT tables** - Initialize new FAT entries for the additional clusters
3. **Updating metadata** - Boot sector and FSInfo must reflect new sizes

`fat32expander` performs these steps, copying data from highest cluster to lowest to avoid overwriting source data.

### Safety Features

- Refuses to operate on mounted filesystems
- Validates boot sector and backup boot sector match
- Verifies filesystem structure before modifications
- Syncs all changes to disk at each phase

### Crash Recovery

If the resize operation is interrupted (power loss, system crash, kill signal), the tool can resume on the next run:

1. **Checkpoint system** - Progress is recorded at each phase
2. **Boot sector invalidation** - During critical operations, the boot sector signature is temporarily set to an invalid value (0x0000), preventing other tools from operating on the inconsistent filesystem
3. **Automatic resume** - Running the tool again detects the incomplete state and continues from the last checkpoint

The operation proceeds in three phases:
- **Phase 0 (Started)**: Data clusters are copied to new positions
- **Phase 1 (DataCopied)**: FAT tables are extended (boot sector invalid during this phase)
- **Phase 2 (FatWritten)**: Boot sector is restored with new parameters

If a crash occurs during Phase 1 (the critical window), the filesystem will appear invalid to other tools until `fat32expander` completes the recovery.

## Testing

The test suite uses QEMU to run tests with real Linux kernel FAT32 drivers:

```bash
# Run all tests
make test-qemu

# Run specific test
make test-qemu-one TEST=test-fragmented

# List available tests
./scripts/qemu/test-qemu.sh --list
```

### Test Scenarios

| Test | Description |
|------|-------------|
| test-simple-resize | Small resize without FAT growth |
| test-fat-growth | Resize requiring FAT table expansion |
| test-4k-sectors | 4096-byte sector filesystem (EFI partition scenario) |
| test-funky-names | Long filenames, unicode, special characters |
| test-deep-hierarchy | 12-level deep directory structure |
| test-near-full | 100% full filesystem |
| test-fragmented | Heavily fragmented files |
| test-large-dir | Directory with 500+ files |

## Requirements

### Runtime
- Linux (uses `/proc/mounts` for mount detection)
- Root privileges (for block devices)

### Building
- Rust 1.70+
- For cross-compilation: appropriate GCC cross-compilers

### Testing
- QEMU with KVM support
- dosfstools (`mkfs.fat`, `fsck.fat`)
- mtools (`mcopy`, `mdir`)

## Limitations

- Linux only (mount detection via `/proc/mounts`)
- Expand only; cannot shrink filesystems (see below)
- Recovery requires running the same tool version that started the operation

### Why No Shrinking?

Expanding is straightforward: shift cluster data forward, keep cluster numbers unchanged, no FAT chain or directory updates needed. Shrinking is fundamentally harder.

When shrinking, clusters beyond the new boundary must be relocated to free space within it. Unlike expanding, this changes cluster numbers, requiring updates to FAT chains and every directory entry pointing to moved files. This means traversing the entire directory tree, parsing entries (including long filename handling), and updating cluster pointers in place.

Additionally, shrinking a filesystem without also shrinking its partition is pointless—partitioning tools won't recognize the reduced size. Supporting both MBR and GPT partition tables adds further complexity.

The result would roughly double the codebase size and risk. Since expanding (growing VM disks, SD cards, disk images) is the common use case, the added complexity isn't justified.

## License

[MIT License](LICENSE)

## Contributing

Before submitting changes:

1. All tests pass: `make test-all`
2. Code is formatted: `make fmt`
3. No clippy warnings: `make lint`

## References

- [Microsoft FAT32 File System Specification](https://download.microsoft.com/download/1/6/1/161ba512-40e2-4cc9-843a-923143f3456c/fatgen103.doc)
- [OSDev Wiki: FAT](https://wiki.osdev.org/FAT)
