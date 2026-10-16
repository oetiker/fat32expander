# Technical Guide: FAT32 Filesystem Expansion

This document explains in detail how `fat32expander` works, the challenges of expanding FAT32 filesystems, and the algorithms used to solve them.

## Table of Contents

1. [FAT32 Filesystem Structure](#fat32-filesystem-structure)
2. [The Expansion Problem](#the-expansion-problem)
3. [The Solution: Data Shifting](#the-solution-data-shifting)
4. [Algorithm Walkthrough](#algorithm-walkthrough)
5. [Code Architecture](#code-architecture)
6. [Edge Cases](#edge-cases)
7. [Crash Recovery](#crash-recovery)

---

## FAT32 Filesystem Structure

### Disk Layout

A FAT32 filesystem has the following structure:

```
┌─────────────┬─────────┬──────────┬───────┬───────┬─────────────────────┐
│ Boot Sector │ FSInfo  │ Reserved │ FAT1  │ FAT2  │ Data Clusters       │
│ (Sector 0)  │ (Sec 1) │ Sectors  │       │       │ (Cluster 2 onwards) │
└─────────────┴─────────┴──────────┴───────┴───────┴─────────────────────┘
     │                        │        │       │
     │                        │        │       └─► File and directory data
     │                        │        └─► Backup copy of FAT1
     │                        └─► File Allocation Table (cluster chain map)
     └─► Contains filesystem parameters
```

### Key Structures

#### Boot Sector (512 bytes)

The boot sector contains critical filesystem parameters:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0x00 | 3 | Jump instruction | Boot code entry |
| 0x0B | 2 | bytes_per_sector | Usually 512 |
| 0x0D | 1 | sectors_per_cluster | 1, 2, 4, 8, 16, 32, or 64 |
| 0x0E | 2 | reserved_sectors | Sectors before FAT1 |
| 0x10 | 1 | num_fats | Usually 2 (FAT1 + FAT2) |
| 0x20 | 4 | total_sectors_32 | Total filesystem size |
| 0x24 | 4 | fat_size_32 | Sectors per FAT |
| 0x2C | 4 | root_cluster | Starting cluster of root directory |
| 0x30 | 2 | fs_info_sector | FSInfo sector number |
| 0x32 | 2 | backup_boot_sector | Backup boot sector location |

#### FSInfo Sector

Contains hints for the operating system:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0x000 | 4 | signature1 | 0x41615252 |
| 0x1E4 | 4 | signature2 | 0x61417272 |
| 0x1E8 | 4 | free_count | Number of free clusters |
| 0x1EC | 4 | next_free | Hint for next free cluster |
| 0x1F0 | 12 | reserved | During a resize only: `RSZP` and the checkpoint sector (u64) |
| 0x1FC | 4 | signature3 | 0xAA550000 |

#### File Allocation Table (FAT)

The FAT is an array of 32-bit entries, one per cluster:

| Value | Meaning |
|-------|---------|
| 0x00000000 | Free cluster |
| 0x00000002 - 0x0FFFFFEF | Next cluster in chain |
| 0x0FFFFFF7 | Bad cluster |
| 0x0FFFFFF8 - 0x0FFFFFFF | End of chain (EOF) |

**Note:** Only the lower 28 bits are used; the upper 4 bits are reserved.

#### Cluster Addressing

- Clusters are numbered starting from 2 (clusters 0 and 1 are reserved)
- Physical sector of cluster N:
  ```
  sector = first_data_sector + (N - 2) * sectors_per_cluster
  ```
- First data sector:
  ```
  first_data_sector = reserved_sectors + (num_fats * fat_size)
  ```

### Directory Entries

Each directory entry is 32 bytes:

| Offset | Size | Field | Description |
|--------|------|-------|-------------|
| 0x00 | 8 | name | Short filename (8 chars) |
| 0x08 | 3 | extension | File extension (3 chars) |
| 0x0B | 1 | attributes | File attributes |
| 0x14 | 2 | cluster_hi | High 16 bits of starting cluster |
| 0x1A | 2 | cluster_lo | Low 16 bits of starting cluster |
| 0x1C | 4 | file_size | File size in bytes |

Long filenames (LFN) use multiple 32-byte entries with attribute 0x0F.

---

## The Expansion Problem

### Why Can't We Just Change Numbers?

When expanding a FAT32 filesystem, we can't simply update the `total_sectors` field in the boot sector. Here's why:

#### Scenario: Small Expansion (No FAT Growth)

If the size increase is small, the existing FAT can track the additional clusters:

```
Before (100 MB):
[Reserved][FAT1: 800 sectors][FAT2: 800 sectors][Data: ~200,000 sectors]

After (110 MB):
[Reserved][FAT1: 800 sectors][FAT2: 800 sectors][Data: ~220,000 sectors]
                                                  └─► Just extend this
```

This is easy - just update `total_sectors` in the boot sector.

#### Scenario: Large Expansion (FAT Must Grow)

If we add many clusters, the FAT needs more entries:

```
Before (100 MB):
[Reserved][FAT1: 800][FAT2: 800][Cluster 2][Cluster 3][Cluster 4]...

After (200 MB) - PROBLEM:
[Reserved][FAT1: 1600][FAT2: 1600][Cluster 2][Cluster 3]...
                 │          │         │
                 └──────────┴─────────┴─► FAT grew INTO data area!
```

The expanded FAT tables now occupy space where data clusters used to be!

### The Core Challenge

When FAT tables grow:

1. **FAT1 expands** into what was the beginning of FAT2
2. **FAT2 moves** to after the new FAT1 end
3. **Data area shifts** forward to after FAT2
4. **All cluster positions change** physically on disk

But cluster NUMBERS don't change - cluster 2 is still cluster 2, it's just at a different sector now.

---

## The Solution: Data Shifting

### Key Insight

Since cluster numbers don't change, we don't need to update FAT chains or directory entries. We just need to physically move all the data to its new location.

### The Algorithm

```
1. Calculate the shift amount (new_first_data_sector - old_first_data_sector)
2. For each cluster from HIGHEST to LOWEST:
   a. Read cluster data from OLD sector position
   b. Write cluster data to NEW sector position
3. Initialize new FAT sectors with zeros (free entries)
4. Copy FAT1 to FAT2
5. Update boot sector with new total_sectors and fat_size
```

### Why Highest to Lowest?

We must copy from highest cluster to lowest to avoid overwriting data:

```
Shift = 2000 sectors

Cluster 1000: old_sector=5000, new_sector=7000  ✓ Safe, 7000 is empty
Cluster 500:  old_sector=2500, new_sector=4500  ✓ Safe, 4500 was cluster 1000's old spot
Cluster 100:  old_sector=2100, new_sector=4100  ✓ Safe, 4100 is in the new FAT area
```

If we went lowest to highest:

```
Cluster 100:  old_sector=2100, new_sector=4100  ✓ OK
Cluster 500:  old_sector=2500, new_sector=4500  ✗ PROBLEM! We just wrote here!
```

---

## Algorithm Walkthrough

### Step 1: Validation

```rust
// Read and validate boot sector
let boot = read_boot_sector(&device)?;

// Ensure backup boot sector matches
let backup = read_backup_boot_sector(&device, boot.backup_boot_sector())?;
if !boot_sectors_match(&boot, &backup) {
    return Err(Error::BackupMismatch);
}

// Check device isn't mounted
check_not_mounted(&device_path)?;
```

### Step 2: Calculate New Sizes

```rust
// Current filesystem parameters
let old_total_sectors = boot.total_sectors();
let old_fat_size = boot.fat_size();
let old_data_clusters = boot.data_clusters();

// Device has more space available
let device_sectors = device.total_sectors();

// Calculate new FAT size needed
// Each FAT entry is 4 bytes, so entries_per_sector = bytes_per_sector / 4
let new_data_clusters = calculate_data_clusters(device_sectors, ...);
let new_fat_size = (new_data_clusters + 2 + entries_per_sector - 1) / entries_per_sector;
```

### Step 3: Determine Affected Range

The "affected range" is the clusters whose old positions will be overwritten by the expanded FAT:

```rust
// Old layout
let old_first_data_sector = reserved + num_fats * old_fat_size;

// New layout
let new_first_data_sector = reserved + num_fats * new_fat_size;

// Shift amount
let shift = new_first_data_sector - old_first_data_sector;

// Clusters 2 through (2 + shift/sectors_per_cluster - 1) are "affected"
let first_affected = 2;
let last_affected = first_affected + (shift / sectors_per_cluster) - 1;
```

### Step 4: Plan Data Movement

```rust
// Find all in-use clusters that need to move
let mut moves = Vec::new();

for cluster in first_affected..old_max_cluster {
    let fat_entry = fat[cluster];

    if !is_free(fat_entry) {
        // This cluster has data that needs to move
        moves.push(ClusterMove {
            cluster,
            old_sector: old_first_data_sector + (cluster - 2) * sectors_per_cluster,
            new_sector: new_first_data_sector + (cluster - 2) * sectors_per_cluster,
        });
    }
}

// Sort by cluster number DESCENDING (highest first)
moves.sort_by(|a, b| b.cluster.cmp(&a.cluster));
```

### Step 5: Execute Data Movement

```rust
for mv in &moves {
    // Read from old position
    let data = device.read_sectors(mv.old_sector, sectors_per_cluster)?;

    // Write to new position
    device.write_sectors(mv.new_sector, &data)?;
}

device.sync()?;
```

### Step 6: Extend FAT Tables

```rust
// Initialize new FAT1 sectors with zeros (free clusters)
let free_sector = vec![0u8; bytes_per_sector];
for offset in old_fat_size..new_fat_size {
    device.write_sector(fat1_start + offset, &free_sector)?;
}

// Copy entire FAT1 to FAT2 (at its new position)
let fat2_start = fat1_start + new_fat_size;
for offset in 0..new_fat_size {
    let data = device.read_sector(fat1_start + offset)?;
    device.write_sector(fat2_start + offset, &data)?;
}
```

### Step 7: Update Metadata

```rust
let boot = boot.with_geometry(new_total_sectors, new_fat_size);

// Backup boot sector, unless the field is 0 (no backup)
if let Some(sector) = boot.backup_location() {
    write_backup_boot_sector(&device, &boot, sector)?;
}

// FSInfo free count, counted from the FAT; the backup FSInfo at
// backup + 1 gets the same count if that sector holds an FSInfo
fsinfo.set_free_count(new_free);
write_fsinfo(&device, &fsinfo, fsinfo_sector)?;
device.sync()?;

// The primary boot sector is written last: it is the commit point
write_boot_sector(&device, &boot)?;
device.sync()?;
```

---

## Code Architecture

### Module Structure

```
src/
├── main.rs              # CLI entry point: clap definitions, main, dispatch
├── cli/                 # Binary-only modules (exit codes, output, resize, version)
├── lib.rs               # Library exports
├── error.rs             # Error types (thiserror)
├── device.rs            # Sector-based device I/O
├── system.rs            # Mount check and device size, per system
├── fat32/
│   ├── mod.rs           # Module exports
│   ├── structs.rs       # BootSector, FSInfo with byte-level accessors
│   ├── validation.rs    # Boot sector and FSInfo validation
│   └── operations.rs    # FAT read/write, cluster operations
└── resize/
    ├── mod.rs           # Module exports
    ├── calculator.rs    # Size calculations for resize
    ├── checkpoint.rs    # On-disk checkpoint format and its sector I/O
    ├── events.rs        # What a resize reports while it runs
    ├── executor.rs      # Main resize orchestration
    ├── fault.rs         # Crash points for the fault-injection feature
    ├── fsinfo.rs        # Read-only filesystem query
    ├── options.rs       # ResizeOptions and ResizeResult
    ├── relocator.rs     # Data shifting logic
    └── testutil.rs      # Shared fixtures for the unit tests
```

### Key Data Structures

```rust
// Boot sector wrapper with accessor methods
pub struct BootSector {
    data: [u8; 512],
}

impl BootSector {
    pub fn bytes_per_sector(&self) -> u16;
    pub fn sectors_per_cluster(&self) -> u8;
    pub fn reserved_sectors(&self) -> u16;
    pub fn num_fats(&self) -> u8;
    pub fn fat_size(&self) -> u32;
    pub fn total_sectors(&self) -> u32;
    pub fn root_cluster(&self) -> u32;
    pub fn first_data_sector(&self) -> u64;
    // `None` when the layout does not fit in the volume, or the cluster is
    // outside it.
    pub fn data_clusters(&self) -> Option<u32>;
    pub fn cluster_to_sector(&self, cluster: u32) -> Option<u64>;
}

// Resize calculation results
pub struct SizeCalculation {
    pub old_total_sectors: u32,
    pub new_total_sectors: u32,
    pub old_fat_size: u32,
    pub new_fat_size: u32,
    pub new_data_clusters: u32,
    pub fat_needs_growth: bool,
    pub first_affected_cluster: u32,
    pub last_affected_cluster: u32,
}

// Planned cluster movement
pub struct ClusterMove {
    pub from_cluster: u32,
    pub to_cluster: u32,    // Same as from_cluster (numbers don't change)
    pub from_sector: u64,   // Old physical position
    pub to_sector: u64,     // New physical position
}
```

---

## Edge Cases

### 1. Filesystem Already at Maximum Size

If `device_sectors <= filesystem_sectors`, there's nothing to expand:

```rust
if device_sectors <= boot.total_sectors() as u64 {
    return Err(Error::NoSpaceToGrow);
}
```

### 2. No FAT Growth Needed

Small expansions may not require FAT growth:

```rust
if new_fat_size <= old_fat_size {
    // Simple case: just update total_sectors
    boot.set_total_sectors_32(new_total_sectors);
    // No data movement needed
}
```

### 3. 100% Full Filesystem

When the filesystem is completely full, there's no free space for relocation targets. However, with the data-shifting approach, this isn't a problem - we're not relocating to different cluster numbers, just moving data to new physical positions.

### 4. Fragmented Files

Files with non-contiguous clusters have FAT chains like:

```
Cluster 5 → Cluster 100 → Cluster 50 → EOF
```

Since we preserve cluster numbers and only change physical positions, FAT chains remain valid without modification.

### 5. Multi-Cluster Directories

Large directories span multiple clusters. The directory's cluster chain is preserved just like any file's chain.

### 6. Root Directory in Affected Range

The root directory starts at `root_cluster` (typically cluster 2). If it's in the affected range, its data moves to a new physical position, but its cluster number stays the same, so no boot sector update is needed.

### 7. Backup Boot Sector

FAT32 maintains a backup boot sector (typically at sector 6) and, in the
sector after it, a backup FSInfo. The resize writes the backup boot sector
first and the primary last. A backup field of 0 means the volume has no
backup, and nothing is written for it. The backup FSInfo gets the new free
count only when its sector lies in the reserved area and already holds an
FSInfo; with the backup in the last reserved sector, the next sector is the
first FAT sector and stays untouched.

### 8. FAT1 and FAT2 Synchronization

FAT32 maintains two copies of the FAT for redundancy. After any changes, both must match:

```rust
// Copy FAT1 to FAT2
for sector in 0..new_fat_size {
    let data = device.read_sector(fat1_start + sector)?;
    device.write_sector(fat2_start + sector, &data)?;
}
```

---

## Crash Recovery

### The Problem

Filesystem resize operations are inherently dangerous. If power is lost or the system crashes mid-operation, the filesystem could be left in an inconsistent state:

- Data partially moved but FAT not updated
- FAT updated but boot sector still has old parameters
- Boot sector updated but backup boot sector doesn't match

### Solution: Checkpoint-Based Recovery

`fat32expander` uses a three-phase checkpoint system to ensure safe recovery from any crash point.

#### Checkpoint Storage

A checkpoint is stored in the **last sector the device had when the resize
started** (beyond the filesystem boundary):

```
┌─────────────────────────────────────────────────────────────────┐
│ [Filesystem (old size)] [Extended space...] [Checkpoint sector] │
└─────────────────────────────────────────────────────────────────┘
                                                      └─► Last sector
```

Right after the first checkpoint is written, and before any data moves, the
sector number is marked in FSInfo's reserved bytes at offset 0x1F0 (see
below). A rerun reads the checkpoint from the marked sector, so it finds it
even if the device has grown since the crash. If the device has shrunk past
the marked sector, the rerun refuses with exit code 9. Without a mark (a crash
before the mark was written, or a run of an older build) the rerun looks in
the device's current last sector. The commit removes the mark as its last
write, after it has cleared the checkpoint, so a crash at any earlier point
still leaves the mark that leads a rerun to the record.

The checkpoint contains:
- Magic signature (`FAT32RSZ`) and format version (bytes 0-8)
- Current phase (Started, DataCopied, FatWritten)
- Old and new filesystem parameters (total_sectors, fat_size), CRC32 at byte 28
- Optionally, from byte 32, the shift progress: `PRG1`, the next cluster to move, CRC32
- From format version 2, at byte 44, the volume serial of the filesystem (boot
  sector offset 67) and its CRC32

A record whose volume serial or old geometry differs from the boot sector's
was not written for this filesystem, so a record left in the last sector of a
reformatted device is never resumed. Behind a valid boot sector such a record
is ignored and a fresh resize overwrites it: every phase that keeps the boot
sector valid also keeps the old geometry and serial in it. Behind an
invalidated boot sector the record is the only way back, so a mismatch is
refused with exit code 9. Version 1 records, which have no serial, are still
read. A record with any other version comes from a newer build and is refused
with exit code 9.

#### The Three Phases

```
Phase 0: Started
   │
   ├─ Write checkpoint, mark its sector in FSInfo, sync
   │
   ▼
┌─────────────────────────────────────┐
│ *** DANGER ZONE ***                 │
│ - Invalidate boot sector (0x0000)   │  ◄── From here on, nothing mounts
│ - Shift clusters forward, highest   │      the volume or writes to it
│   first, recording progress         │
│   (old homes are overwritten)       │
│                                     │
│ Phase 1: DataCopied                 │
│ - Extend FAT1, copy it to FAT2      │
│ - Sync                              │
│                                     │
│ Phase 2: FatWritten                 │
│ - Write backup boot sector          │
│ - Write FSInfo free count (mark     │
│   kept) and backup FSInfo, sync     │
│ - Write primary boot sector: new    │  ◄── Commit point
│   geometry, signature 0xAA55, sync  │
└─────────────────────────────────────┘
   │
   ├─ Clear checkpoint, sync
   ├─ Remove the mark from FSInfo, sync
   ▼
Complete
```

A resize without FAT growth skips phases 0 and 1. Its commit first writes a
`FatWritten` checkpoint and marks it in FSInfo, then runs phase 2 behind a
boot sector that stays valid until the primary write.

### Boot Sector Invalidation

The **danger zone** runs from just before the first cluster moves to the
write of the primary boot sector. During it the boot sector signature is
`0x0000` instead of `0xAA55`.

The shift overwrites the old home of each cluster it moves. Behind a valid
boot sector, a crash during the shift would leave a volume that mounts with
the old geometry and partly overwritten files, and anything written to it
before the rerun would destroy data the resume still has to copy.

The invalidated signature therefore serves two purposes:

1. **Protection from other tools**: after a crash, `fsck.fat` and the
   kernel's FAT driver refuse the volume, so nothing writes to it before
   the rerun.

2. **Recovery detection**: when `fat32expander` runs again, it detects the
   invalidated signature and knows recovery is needed.

```rust
// Enter the danger zone: checkpoint and FSInfo mark are durable already.
boot.invalidate_signature();  // Sets bytes 510-511 to 0x0000
write_boot_sector(&device, &boot)?;
device.sync()?;

// ... shift clusters, extend the FAT, write backup boot sector and FSInfo ...

// Leave the danger zone: this write is the commit point.
let mut boot = boot.with_geometry(new_total_sectors, new_fat_size);
boot.restore_signature();     // Sets bytes 510-511 to 0xAA55
write_boot_sector(&device, &boot)?;
```

### Recovery Algorithm

On startup, `fat32expander` looks for an interrupted resize:

1. It reads the checkpoint from the sector marked in FSInfo, or from the
   device's last sector if FSInfo carries no mark. A marked sector past the
   end of the device is refused with exit code 9.
2. Behind an **invalidated** boot sector, a checkpoint must exist and must
   match the volume serial and old geometry of the boot sector. A missing or
   mismatching record is refused with exit code 9: it is the only way back.
3. Behind a **valid** boot sector, a matching checkpoint is resumed. A
   mismatching one belongs to an earlier filesystem on the device; it is
   reported as ignored and a fresh resize overwrites it.
4. A resume whose device is now smaller than the checkpoint's new size is
   refused with exit code 9. A larger device is fine: the resume finishes the
   size it was interrupted at, and a second run grows the rest.

Recovery behaviour depends on where the crash happened. Each row is a crash
point of the fault-injection suite:

| Crash point | Phase | Boot sector | Recovery action |
|-------------|-------|-------------|-----------------|
| `after_checkpoint_start` | Started | Valid (0xAA55) | Shift from its first cluster |
| `before_mid_shift_record` | Started | Invalid (0x0000) | Resume the shift from the recorded progress |
| `after_data_shift` | Started | Invalid (0x0000) | Resume the shift from the recorded progress |
| `after_checkpoint_data_copied` | DataCopied | Invalid (0x0000) | Skip the shift, write the FAT extension |
| `after_boot_invalidate` | DataCopied | Invalid (0x0000) | Skip the shift, write the FAT extension |
| `after_fat_write` | DataCopied | Invalid (0x0000) | Skip the shift, write the FAT extension again |
| `after_checkpoint_fat_written` | FatWritten | Invalid (0x0000) | Commit only |
| `before_primary_boot_write` | FatWritten | Invalid (0x0000) | Commit only |

`after_checkpoint_data_copied` and `after_boot_invalidate` stop a fresh run
at the same moment; only a resume from `DataCopied` passes
`after_boot_invalidate` alone. Without FAT growth, `before_primary_boot_write`
leaves a valid boot sector and a `FatWritten` checkpoint, and the rerun
commits only.

### Resumable, Not Idempotent

The data shift is **not** idempotent. Running it twice over the same device
corrupts data, so recovery resumes it from a recorded position rather than
replaying it.

Clusters are copied highest-first, so the destination of cluster `c` is the old
home of cluster `c + k`, where `k` is the shift distance in clusters. With
`k = 2` and clusters 2, 3, 4 occupying sectors 100, 101, 102:

```
Start:     100=c2  101=c3  102=c4
After 1x:  100=c2  101=c3  102=c2  103=c3  104=c4   <- correct
Restart:   moving c4 reads sector 102, which now holds c2, and writes that
           to 104. c4 is destroyed.
```

A first pass is safe because `c + k` has already been moved by the time `c` is
written. That same overlap is what makes a restart unsafe: `old_home(c)` is
overwritten by the move of `c - k`, which happens *later* in the descending
pass.

The claim this section used to make - that source data is copied rather than
moved, so the original is preserved - holds only while the old and new data
ranges are disjoint, i.e. while `k` exceeds the highest used cluster. On any
filesystem with more than `k` clusters in use it is false. The crash-recovery
suite could not detect this because its geometry kept the ranges disjoint; see
`assert_overlapping_shift` in `scripts/qemu/test-crash-recovery.sh`, which now
fails the suite if that ever becomes true again.

Recovery therefore records progress:

1. Before any move that lies `min(1024, k)` clusters or more below the last
   record, the checkpoint records the next cluster still to move. The distance
   is counted in clusters, not moves: free clusters get no move, so counting
   moves would let a batch on a fragmented filesystem descend further than `k`.
2. The preceding batch is synced before that record advances, so the record
   never runs ahead of the data.
3. A resume skips every move above the recorded cluster.

Replaying the moves within `k` clusters of the record is harmless -
`old_home(c)` survives until the pass descends `k` clusters below it - which is
why progress need not be recorded after every single move.

If a checkpoint carries no progress record the position is unknown, and the
resume is **refused** rather than guessed.

Every shift moves the data by at least one cluster (`k >= 1`). At `k == 0` a
cluster's destination overlaps its own source, so no move could be replayed and
a crash during the shift could not be resumed. When the FAT would grow by less
than one cluster in total, the calculator grows it further, which costs less
than one cluster of space [F-T01]. The data shift refuses a plan with `k == 0`
before its first write.

### Testing Crash Recovery

The test suite uses **fault injection** to verify recovery at all crash points:

```bash
# Build with fault injection (testing only)
cargo build --features fault-injection

# Trigger crash at specific point
FAT32_CRASH_AT=after_boot_invalidate ./fat32expander resize image.img
```

Available crash points:
- `after_checkpoint_start`
- `before_mid_shift_record` (part-way through the data shift, before the
  second progress record; fires only when the shift records progress more
  than once)
- `after_data_shift`
- `after_checkpoint_data_copied`
- `after_boot_invalidate`
- `after_fat_write`
- `after_checkpoint_fat_written`
- `before_primary_boot_write`

**Note:** Fault injection is only available when built with `--features fault-injection` and is never included in production builds.

---

## Performance Considerations

### I/O Efficiency

- Clusters are read/written in full (all sectors_per_cluster sectors at once)
- Operations are performed sequentially from highest to lowest cluster
- Device sync is called after major phases to ensure durability

### Memory Usage

- FAT table is read entirely into memory for analysis
- For a 2TB filesystem with 512-byte clusters, FAT is ~16GB
- For typical use cases (< 32GB), FAT is < 128MB

### Time Complexity

- **Best case** (no FAT growth): O(1) - just update boot sector
- **Worst case** (FAT growth): O(n) where n = number of in-use clusters

---

## References

- [Microsoft FAT32 File System Specification](https://download.microsoft.com/download/1/6/1/161ba512-40e2-4cc9-843a-923143f3456c/fatgen103.doc)
- [OSDev Wiki: FAT](https://wiki.osdev.org/FAT)
- [Linux kernel vfat driver](https://github.com/torvalds/linux/tree/master/fs/fat)
