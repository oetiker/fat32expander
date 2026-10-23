# fat32expander

[![CI](https://github.com/oetiker/fat32expander/actions/workflows/ci.yml/badge.svg)](https://github.com/oetiker/fat32expander/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/oetiker/fat32expander)](https://github.com/oetiker/fat32expander/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A command-line tool to expand FAT32 filesystems in-place after their underlying partition or disk image has been grown.

## Warning

**Filesystem modification is inherently risky.** While this tool includes safeguards, data loss is possible due to hardware failure, power loss, bugs, or unexpected conditions. Always maintain backups of important data before modifying any filesystem.

## Overview

When you resize a partition (for example with `parted`, `fdisk`, or by expanding a virtual disk), the filesystem inside does not automatically grow to fill the new space. `fat32expander` handles this for FAT32 filesystems, including the case where the FAT tables need to grow, which requires relocating data clusters.

## Features

- **In-place expansion**: modifies the filesystem directly, without temporary storage
- **FAT table growth**: relocates clusters when the FAT tables need to expand
- **Multiple sector sizes**: 512, 1024, 2048 and 4096-byte sectors (including EFI partitions on 4Kn drives)
- **Crash recovery**: an interrupted resize continues on the next run
- **Dry-run mode**: preview changes without modifying the filesystem
- **Verbose output**: each step of the resize as it runs

## Installation

Debian and Ubuntu:

```sh
sudo install -d -m 0755 /etc/apt/keyrings
sudo curl -fsSL -o /etc/apt/keyrings/gitea-oposs.asc https://gitea.oetiker.ch/api/packages/oposs/debian/repository.key
echo "deb [signed-by=/etc/apt/keyrings/gitea-oposs.asc] https://gitea.oetiker.ch/api/packages/oposs/debian stable main" | sudo tee /etc/apt/sources.list.d/oposs.list
sudo apt update && sudo apt install fat32expander
```

Fedora, RHEL, Rocky and Alma:

```sh
sudo curl -fsSL -o /etc/yum.repos.d/oposs.repo https://gitea.oetiker.ch/api/packages/oposs/rpm.repo
sudo dnf install fat32expander
```

Homebrew (macOS and Linux):

```sh
brew tap oetiker/fat32expander https://github.com/oetiker/fat32expander
brew trust --formula oetiker/fat32expander/fat32expander
brew install fat32expander
```

`brew trust` exists from Homebrew 6 on; skip it on older versions.

mise:

```sh
mise use -g github:oetiker/fat32expander
```

Direct download: static binaries for Linux (x86_64, arm64), macOS (Intel,
Apple Silicon) and illumos on the
[releases page](https://github.com/oetiker/fat32expander/releases/latest).

### Building from source

Requires a current stable Rust.

```bash
git clone https://github.com/oetiker/fat32expander.git
cd fat32expander

# Release binary for this machine
make release

# Static Linux x86_64 binary
make static
```

The manual is `docs/manual.md`; `make man` builds the man page
`man/fat32expander.8` from it.

### Cross-compilation

```bash
# Install Rust targets
make setup-targets

# Build for specific platforms
make linux-x64      # Linux x86_64 (static)
make linux-arm64    # Linux ARM64 (static)
make macos-x64      # macOS Intel
make macos-arm64    # macOS Apple Silicon

# Build the Linux binaries into dist/
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

### Exit Codes

Failures are reported with distinct exit codes, so a calling script can tell
them apart without parsing messages. The codes are grouped by what the caller
should do about the failure. The manual lists them under
[EXIT STATUS](docs/manual.md#exit-status).

`resize` is safe to re-run: a filesystem that is already at its maximum size
exits 0 rather than reporting an error, so the tool can be called
unconditionally from a provisioning script.

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

- Refuses to operate on mounted filesystems, including an image file mounted through a loop device, and holds a block device exclusively while it resizes
- Validates boot sector and backup boot sector match
- Refuses a volume whose FAT mirroring is off with an active FAT other than the first, and a FAT32 version other than 0.0
- Verifies filesystem structure before modifications
- Syncs all changes to disk at each phase

### Crash Recovery

If the resize operation is interrupted (power loss, system crash, kill signal), the tool can resume on the next run:

1. **Checkpoint system** - Progress is recorded at each phase
2. **Boot sector invalidation** - Before the first data cluster moves, the boot sector signature is set to an invalid value (0x0000), so no other tool mounts or writes to the filesystem while it is inconsistent
3. **Automatic resume** - Running the tool again detects the incomplete state and continues from the last checkpoint

The operation proceeds in three phases:
- **Phase 0 (Started)**: Data clusters are moved to their new positions, overwriting their old ones
- **Phase 1 (DataCopied)**: FAT tables are extended
- **Phase 2 (FatWritten)**: The backup boot sector and FSInfo are updated, then the boot sector is written with the new parameters and a valid signature

The boot sector stays invalid from the start of Phase 0 until the end of Phase 2. A crash in between leaves a filesystem that other tools treat as invalid until `fat32expander` completes the recovery.

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
- Linux, macOS or illumos. CI tests Linux and macOS on real disk devices;
  illumos builds, but no test has run on an illumos system yet.
- Write access to the target device. That usually means root for a block
  device, but the tool does not require root as such: membership of `disk`,
  a suitable ACL, or owning the image file all work.

### Building
- A current stable Rust
- For cross-compilation: appropriate GCC cross-compilers
- For the man page: pandoc

### Testing
- QEMU with KVM support
- dosfstools (`mkfs.fat`, `fsck.fat`)
- mtools (`mcopy`, `mdir`)

`make ci-container` runs the full CI test suite in a container that has all
of these, with podman or docker.

## Limitations

- Linux, macOS and illumos only. Windows is not supported.
- On macOS and illumos the device is not locked during a resize: nothing
  stops a mount after the mount check has passed. Keep the volume unmounted
  until the resize has finished.
- On macOS, pass the disk device (`/dev/diskN` or `/dev/rdiskN`), not the
  image file of an attached disk image: a mounted image is detected only
  through its disk device.
- On illumos, a FAT volume mounted through the `p0:c` logical-drive syntax of
  `pcfs` is not detected as mounted. Unmount it before a resize.
- Expand only; cannot shrink filesystems (see below)
- Recovery requires running the same tool version that started the operation

### Why No Shrinking?

Expanding is straightforward: shift cluster data forward, keep cluster numbers unchanged, no FAT chain or directory updates needed. Shrinking is fundamentally harder.

When shrinking, clusters beyond the new boundary must be relocated to free space within it. Unlike expanding, this changes cluster numbers, requiring updates to FAT chains and every directory entry pointing to moved files. This means traversing the entire directory tree, parsing entries (including long filename handling), and updating cluster pointers in place.

Additionally, shrinking a filesystem without also shrinking its partition is pointless: partitioning tools won't recognize the reduced size. Supporting both MBR and GPT partition tables adds further complexity.

The result would roughly double the codebase size and risk. Since expanding (growing VM disks, SD cards, disk images) is the common use case, the added complexity isn't justified.

## License

[MIT License](LICENSE)

## Contributing

Before submitting changes:

1. All tests pass: `make test` and `make ci-container`
2. Code is formatted: `make fmt`
3. No clippy warnings: `make lint`

## References

- [Microsoft FAT32 File System Specification](https://download.microsoft.com/download/1/6/1/161ba512-40e2-4cc9-843a-923143f3456c/fatgen103.doc)
- [OSDev Wiki: FAT](https://wiki.osdev.org/FAT)
