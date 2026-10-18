---
title: FAT32EXPANDER
section: 8
header: fat32expander manual
footer: fat32expander
date: 2026-10-18
---

# NAME

fat32expander - grow a FAT32 filesystem in place to fill its partition

# SYNOPSIS

**fat32expander** **resize** [**-n**] [**-v**] [**\-\-ignore-backup-mismatch**] *device*

**fat32expander** **info** *device*

**fat32expander** **version**

**fat32expander** **-V** | **-h**

# DESCRIPTION

**fat32expander** enlarges a FAT32 filesystem after its partition, disk or image file has grown.
The filesystem grows to the size of *device*.
*device* is a block device or an image file.

When the FAT tables must grow to address the new clusters, the data area starts later on the device.
**fat32expander** then moves every data cluster forward by the same number of sectors.
Cluster numbers do not change, so the FAT chains and directory entries stay as they are.

A resize writes a checkpoint before each phase.
Before the first cluster moves, it sets the boot sector signature to an invalid value, so no other program mounts the filesystem while it is inconsistent.
When a resize is interrupted, for example by a power loss, the next **fat32expander resize** on the same device reads the checkpoint and finishes the resize.
The filesystem stays unmountable until then.

**fat32expander** refuses a device that is mounted, also through a loop device, or that another program holds open.
On Linux it holds a block device exclusively for the length of the resize.
It refuses a filesystem whose backup boot sector differs from the primary, unless **\-\-ignore-backup-mismatch** is given.
It does not shrink a filesystem.

The device must be writable by the calling user.
For a block device that is usually root, but group membership or an ACL suffices.

# OPTIONS

- `-n, --dry-run`: With **resize**, print what the resize would do and change nothing.
- `-v, --verbose`: With **resize**, print each step while it runs.
- `--ignore-backup-mismatch`: With **resize**, proceed when the backup boot sector differs from the primary. The resize trusts the primary and rewrites the backup from it. If the primary is the damaged copy, this discards the only good record of the original layout.
- `-V, --version`: Print the version and exit.
- `-h, --help`: Print a usage summary and exit.

# COMMANDS

- `resize <device>`: Grow the filesystem on *device* to the size of *device*, or finish an interrupted resize. A filesystem already at its largest size is left unchanged; **fat32expander** prints "Nothing to do." and exits 0.
- `info <device>`: Print the layout and free space of the filesystem on *device*, and the size it can grow to. Changes nothing.
- `version`: Print the version and build information.
- `help [command]`: Print a usage summary of **fat32expander** or of *command*.

# EXIT STATUS

- `0`: Success, also a resize that found nothing to do.
- `1`: Unexpected internal failure.
- `2`: Invalid command line.
- `3`: The device could not be opened. The path, the permissions or another program holding the device are the usual causes.
- `4`: The device is mounted, also through a loop device, or another program holds it.
- `5`: The device holds no usable FAT32 filesystem.
- `7`: The device is smaller than the filesystem claims: a truncated image or the wrong device.
- `8`: The backup boot sector differs from the primary. **\-\-ignore-backup-mismatch** trusts the primary.
- `9`: An interrupted resize needs recovery that **fat32expander** cannot do on its own.
- `10`: Reading or writing the device failed.
- `12`: The mount table could not be read, so the mount check could not run.
- `13`: The grown filesystem would have more clusters than FAT32 allows (268,435,445). The filesystem needs a larger cluster size for this device.

Codes 6 and 11 are no longer used.

# NOTES

Sector sizes of 512, 1024, 2048 and 4096 bytes are supported.

On macOS and illumos the device is not locked during a resize, and nothing stops a mount after the mount check.
The volume must stay unmounted until the resize has finished.

On macOS the device is the disk device, */dev/diskN* or */dev/rdiskN*, and not the image file of an attached disk image.
A mounted image is detected through its disk device only.

On illumos a FAT volume mounted through the **p0:c** logical-drive syntax of **pcfs** is not detected as mounted.

An interrupted resize is finished by the same version of **fat32expander** that started it.

# EXAMPLES

Grow partition 1 of */dev/sdb* to the end of the disk, then grow its filesystem:

```sh
parted /dev/sdb resizepart 1 100%
fat32expander resize /dev/sdb1
```

Show what a resize of an image file would do after the file was enlarged:

```sh
truncate -s 2G disk.img
fat32expander resize --dry-run disk.img
```

# AUTHORS

Tobias Oetiker <tobi@oetiker.ch>,
Claude (Anthropic).

# REPORTING BUGS

<https://github.com/oetiker/fat32expander/issues>

# COPYRIGHT

Copyright (c) 2024 Tobias Oetiker.
Released under the MIT licence, see
<https://github.com/oetiker/fat32expander/blob/main/LICENSE>.

# SEE ALSO

Project home: <https://github.com/oetiker/fat32expander>

**mkfs.fat**(8),
**fsck.fat**(8),
**parted**(8)
