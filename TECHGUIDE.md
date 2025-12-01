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
// Update boot sector
boot.set_total_sectors_32(new_total_sectors);
boot.set_fat_size_32(new_fat_size);
write_boot_sector(&device, &boot)?;

// Update backup boot sector
write_backup_boot_sector(&device, &boot, backup_sector)?;

// Update FSInfo
let additional_clusters = new_data_clusters - old_data_clusters;
fsinfo.set_free_count(old_free + additional_clusters);
write_fsinfo(&device, &fsinfo, fsinfo_sector)?;

device.sync()?;
```

---

## Code Architecture

### Module Structure

```
src/
├── main.rs              # CLI entry point (clap-based)
├── lib.rs               # Library exports
├── error.rs             # Error types (thiserror)
├── device.rs            # Sector-based device I/O
├── system.rs            # Mount detection via /proc/mounts
├── fat32/
│   ├── mod.rs           # Module exports
│   ├── structs.rs       # BootSector, FSInfo with byte-level accessors
│   ├── validation.rs    # Boot sector and FSInfo validation
│   └── operations.rs    # FAT read/write, cluster operations
└── resize/
    ├── mod.rs           # Module exports
    ├── calculator.rs    # Size calculations for resize
    ├── relocator.rs     # Data shifting logic
    └── executor.rs      # Main resize orchestration
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
    pub fn data_clusters(&self) -> u32;
    pub fn cluster_to_sector(&self, cluster: u32) -> u64;
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

FAT32 maintains a backup boot sector (typically at sector 6). Both must be updated:

```rust
write_boot_sector(&device, &boot)?;           // Primary
write_backup_boot_sector(&device, &boot, 6)?; // Backup
```

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

A checkpoint is stored in the **last sector of the device** (beyond the filesystem boundary):

```
┌─────────────────────────────────────────────────────────────────┐
│ [Filesystem (old size)] [Extended space...] [Checkpoint sector] │
└─────────────────────────────────────────────────────────────────┘
                                                      └─► Last sector
```

The checkpoint contains:
- Magic signature (0xFA32CHKP)
- Current phase (Started, DataCopied, FatWritten)
- Old and new filesystem parameters (total_sectors, fat_size)

#### The Three Phases

```
Phase 0: Started
   │
   ▼
┌─────────────────────────────────────┐
│ Data Shift (copy clusters forward)  │  ◄── Safe: source data preserved
└─────────────────────────────────────┘
   │
   ▼
Phase 1: DataCopied
   │
   ▼
┌─────────────────────────────────────┐
│ *** DANGER ZONE ***                 │
│ - Invalidate boot sector (0x0000)   │  ◄── Prevents other tools from
│ - Extend FAT tables                 │      operating on corrupt state
│ - Sync FAT1 to FAT2                 │
└─────────────────────────────────────┘
   │
   ▼
Phase 2: FatWritten
   │
   ▼
┌─────────────────────────────────────┐
│ - Restore boot sector (0xAA55)      │
│ - Update boot sector parameters     │
│ - Update backup boot sector         │
│ - Update FSInfo                     │
│ - Clear checkpoint                  │
└─────────────────────────────────────┘
   │
   ▼
Complete
```

### Boot Sector Invalidation

The **danger zone** is the period when the filesystem is in an inconsistent state (FAT tables being modified). During this period, the boot sector signature is changed from `0xAA55` to `0x0000`.

This serves two purposes:

1. **Protection from other tools**: If the system crashes and reboots, tools like `fsck.fat` or the OS's FAT driver will refuse to mount or operate on the "invalid" filesystem.

2. **Recovery detection**: When `fat32expander` runs again, it detects the invalidated signature and knows recovery is needed.

```rust
// Enter danger zone
boot.invalidate_signature();  // Sets bytes 510-511 to 0x0000
write_boot_sector(&device, &boot)?;
device.sync()?;

// ... perform FAT operations ...

// Exit danger zone
boot.restore_signature();     // Sets bytes 510-511 to 0xAA55
boot.set_total_sectors_32(new_total_sectors);
boot.set_fat_size_32(new_fat_size);
write_boot_sector(&device, &boot)?;
```

### Recovery Algorithm

On startup, `fat32expander` checks for incomplete operations:

```rust
fn check_for_incomplete_resize(device, boot) -> Option<Checkpoint> {
    if boot.signature == 0x0000 {
        // Boot sector invalidated - MUST recover
        // Read checkpoint from last device sector
        return read_checkpoint(device);
    } else if device_size > filesystem_size {
        // Check for phase 0 crash (before invalidation)
        if let Some(checkpoint) = read_checkpoint(device) {
            return Some(checkpoint);
        }
    }
    None
}
```

Recovery behavior depends on the checkpoint phase:

| Crash Point | Phase | Boot Sector | Recovery Action |
|-------------|-------|-------------|-----------------|
| After checkpoint write | Started | Valid (0xAA55) | Re-run entire resize |
| After data shift | Started | Valid (0xAA55) | Re-run data shift (idempotent) |
| After phase 1 checkpoint | DataCopied | Valid (0xAA55) | Skip data shift, continue |
| After boot invalidation | DataCopied | Invalid (0x0000) | Skip data shift, continue |
| After FAT write | DataCopied | Invalid (0x0000) | Continue from FAT sync |
| After phase 2 checkpoint | FatWritten | Invalid (0x0000) | Just restore boot sector |

### Idempotent Operations

The data shift operation is designed to be **idempotent** - running it multiple times produces the same result:

```
Original:  [FAT][Cluster2 @ sector 100][Cluster3 @ sector 101]...
After 1x:  [FAT][...][Cluster2 @ sector 200][Cluster3 @ sector 201]...
After 2x:  [FAT][...][Cluster2 @ sector 200][Cluster3 @ sector 201]...  (same)
```

This works because:
1. Source data is **copied**, not moved (original preserved)
2. We always read from the **old** sector position
3. We always write to the **new** sector position

If a crash happens during data shift and we restart, re-running the shift just copies the same data again.

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
- `after_data_shift`
- `after_checkpoint_data_copied`
- `after_boot_invalidate`
- `after_fat_write`
- `after_checkpoint_fat_written`

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
