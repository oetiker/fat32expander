use crate::error::{Error, Result};

// ===== Newtype Wrappers for Type Safety =====

/// A cluster ID in a FAT32 filesystem.
///
/// Cluster IDs start at 2 (clusters 0 and 1 are reserved).
/// This newtype prevents accidentally mixing up cluster IDs with sector numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClusterId(u32);

impl ClusterId {
    /// The first valid data cluster (cluster 2)
    pub const FIRST_DATA_CLUSTER: Self = Self(2);

    /// Create a new ClusterId from a raw value.
    ///
    /// Note: This does not validate that the cluster ID is >= 2.
    /// Use `new_checked` for validation.
    #[inline]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// Create a new ClusterId with validation.
    ///
    /// Returns `None` if the cluster ID is less than 2 (reserved).
    #[inline]
    pub const fn new_checked(id: u32) -> Option<Self> {
        if id >= 2 {
            Some(Self(id))
        } else {
            None
        }
    }

    /// Get the raw cluster ID value.
    #[inline]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Get the cluster index (0-based offset from cluster 2).
    ///
    /// This is useful for calculating sector offsets.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0.saturating_sub(2)
    }
}

impl From<u32> for ClusterId {
    fn from(id: u32) -> Self {
        Self(id)
    }
}

impl From<ClusterId> for u32 {
    fn from(id: ClusterId) -> Self {
        id.0
    }
}

impl std::fmt::Display for ClusterId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cluster {}", self.0)
    }
}

/// A sector number in a FAT32 filesystem.
///
/// This newtype prevents accidentally mixing up sector numbers with cluster IDs
/// or byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SectorNum(u64);

impl SectorNum {
    /// Create a new SectorNum from a raw value.
    #[inline]
    pub const fn new(sector: u64) -> Self {
        Self(sector)
    }

    /// Get the raw sector number.
    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Add an offset to this sector number.
    #[inline]
    pub const fn offset(self, offset: u64) -> Self {
        Self(self.0 + offset)
    }

    /// Calculate byte offset from start of device given a sector size.
    #[inline]
    pub const fn to_byte_offset(self, sector_size: u32) -> u64 {
        self.0 * sector_size as u64
    }
}

impl From<u64> for SectorNum {
    fn from(sector: u64) -> Self {
        Self(sector)
    }
}

impl From<SectorNum> for u64 {
    fn from(sector: SectorNum) -> Self {
        sector.0
    }
}

impl std::fmt::Display for SectorNum {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sector {}", self.0)
    }
}

// ===== FAT32 Structures =====

/// FAT32 Boot Sector / BIOS Parameter Block
///
/// This structure represents the first sector of a FAT32 filesystem.
/// All multi-byte values are stored in little-endian format.
/// Supports sector sizes of 512, 1024, 2048, or 4096 bytes.
#[derive(Clone)]
pub struct BootSector {
    /// Full sector data (512 to 4096 bytes depending on sector size)
    raw: Vec<u8>,
}

impl BootSector {
    /// Parse a boot sector from raw bytes
    /// The input must be at least 512 bytes (the minimum sector size)
    /// and will be stored in full for proper read-modify-write cycles
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 512 {
            return Err(Error::BootSectorValidation(format!(
                "Boot sector too small: {} bytes",
                bytes.len()
            )));
        }

        // Store the full sector data for read-modify-write
        Ok(Self {
            raw: bytes.to_vec(),
        })
    }

    /// Get the raw bytes (full sector)
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Get mutable raw bytes
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.raw
    }

    /// Get the sector size this boot sector was read with
    pub fn sector_size(&self) -> usize {
        self.raw.len()
    }

    // ===== BPB Fields (BIOS Parameter Block) =====

    /// Jump instruction (offset 0, 3 bytes)
    pub fn jump_boot(&self) -> &[u8] {
        &self.raw[0..3]
    }

    /// OEM Name (offset 3, 8 bytes)
    pub fn oem_name(&self) -> &[u8] {
        &self.raw[3..11]
    }

    /// Bytes per sector (offset 11, 2 bytes) - typically 512
    pub fn bytes_per_sector(&self) -> u16 {
        u16::from_le_bytes([self.raw[11], self.raw[12]])
    }

    /// Sectors per cluster (offset 13, 1 byte)
    pub fn sectors_per_cluster(&self) -> u8 {
        self.raw[13]
    }

    /// Reserved sector count (offset 14, 2 bytes) - includes boot sector
    pub fn reserved_sectors(&self) -> u16 {
        u16::from_le_bytes([self.raw[14], self.raw[15]])
    }

    /// Number of FAT copies (offset 16, 1 byte) - typically 2
    pub fn num_fats(&self) -> u8 {
        self.raw[16]
    }

    /// Root directory entries for FAT12/16 (offset 17, 2 bytes) - 0 for FAT32
    pub fn root_entry_count(&self) -> u16 {
        u16::from_le_bytes([self.raw[17], self.raw[18]])
    }

    /// Total sectors 16-bit for FAT12/16 (offset 19, 2 bytes) - 0 for FAT32
    pub fn total_sectors_16(&self) -> u16 {
        u16::from_le_bytes([self.raw[19], self.raw[20]])
    }

    /// Media type (offset 21, 1 byte) - 0xF8 for hard disks
    pub fn media_type(&self) -> u8 {
        self.raw[21]
    }

    /// Sectors per FAT for FAT12/16 (offset 22, 2 bytes) - 0 for FAT32
    pub fn fat_size_16(&self) -> u16 {
        u16::from_le_bytes([self.raw[22], self.raw[23]])
    }

    /// Sectors per track (offset 24, 2 bytes)
    pub fn sectors_per_track(&self) -> u16 {
        u16::from_le_bytes([self.raw[24], self.raw[25]])
    }

    /// Number of heads (offset 26, 2 bytes)
    pub fn num_heads(&self) -> u16 {
        u16::from_le_bytes([self.raw[26], self.raw[27]])
    }

    /// Hidden sectors (offset 28, 4 bytes) - sectors before this partition
    pub fn hidden_sectors(&self) -> u32 {
        u32::from_le_bytes([self.raw[28], self.raw[29], self.raw[30], self.raw[31]])
    }

    /// Total sectors 32-bit (offset 32, 4 bytes)
    pub fn total_sectors_32(&self) -> u32 {
        u32::from_le_bytes([self.raw[32], self.raw[33], self.raw[34], self.raw[35]])
    }

    /// Set total sectors 32-bit
    pub fn set_total_sectors_32(&mut self, sectors: u32) {
        let bytes = sectors.to_le_bytes();
        self.raw[32..36].copy_from_slice(&bytes);
    }

    // ===== FAT32 Extended BPB Fields =====

    /// Sectors per FAT for FAT32 (offset 36, 4 bytes)
    pub fn fat_size_32(&self) -> u32 {
        u32::from_le_bytes([self.raw[36], self.raw[37], self.raw[38], self.raw[39]])
    }

    /// Set sectors per FAT for FAT32
    pub fn set_fat_size_32(&mut self, size: u32) {
        let bytes = size.to_le_bytes();
        self.raw[36..40].copy_from_slice(&bytes);
    }

    /// Extended flags (offset 40, 2 bytes)
    /// Bit 7: 0 = FAT is mirrored, 1 = only one FAT is active
    /// Bits 0-3: Active FAT number (if bit 7 is 1)
    pub fn ext_flags(&self) -> u16 {
        u16::from_le_bytes([self.raw[40], self.raw[41]])
    }

    /// FAT32 version (offset 42, 2 bytes) - typically 0.0
    pub fn fs_version(&self) -> u16 {
        u16::from_le_bytes([self.raw[42], self.raw[43]])
    }

    /// Root directory cluster (offset 44, 4 bytes) - typically 2
    pub fn root_cluster(&self) -> u32 {
        u32::from_le_bytes([self.raw[44], self.raw[45], self.raw[46], self.raw[47]])
    }

    /// Set root directory cluster
    pub fn set_root_cluster(&mut self, cluster: u32) {
        let bytes = cluster.to_le_bytes();
        self.raw[44..48].copy_from_slice(&bytes);
    }

    /// FSInfo sector number (offset 48, 2 bytes) - typically 1
    pub fn fs_info_sector(&self) -> u16 {
        u16::from_le_bytes([self.raw[48], self.raw[49]])
    }

    /// Backup boot sector location (offset 50, 2 bytes) - typically 6
    pub fn backup_boot_sector(&self) -> u16 {
        u16::from_le_bytes([self.raw[50], self.raw[51]])
    }

    /// Reserved (offset 52, 12 bytes)
    pub fn reserved(&self) -> &[u8] {
        &self.raw[52..64]
    }

    /// Drive number (offset 64, 1 byte)
    pub fn drive_number(&self) -> u8 {
        self.raw[64]
    }

    /// Reserved1/Windows NT flags (offset 65, 1 byte)
    pub fn reserved1(&self) -> u8 {
        self.raw[65]
    }

    /// Boot signature (offset 66, 1 byte) - 0x28 or 0x29
    pub fn boot_sig(&self) -> u8 {
        self.raw[66]
    }

    /// Volume serial number (offset 67, 4 bytes)
    pub fn volume_id(&self) -> u32 {
        u32::from_le_bytes([self.raw[67], self.raw[68], self.raw[69], self.raw[70]])
    }

    /// Volume label (offset 71, 11 bytes)
    pub fn volume_label(&self) -> &[u8] {
        &self.raw[71..82]
    }

    /// File system type string (offset 82, 8 bytes) - "FAT32   "
    pub fn fs_type(&self) -> &[u8] {
        &self.raw[82..90]
    }

    /// Boot signature at end of sector (offset 510, 2 bytes) - must be 0xAA55
    pub fn boot_signature(&self) -> u16 {
        u16::from_le_bytes([self.raw[510], self.raw[511]])
    }

    /// Valid boot sector signature value
    pub const VALID_SIGNATURE: u16 = 0xAA55;

    /// Check if the boot sector signature is valid
    pub fn is_signature_valid(&self) -> bool {
        self.boot_signature() == Self::VALID_SIGNATURE
    }

    /// Invalidate the boot sector signature (for crash safety during resize)
    /// This prevents other tools from operating on the filesystem during dangerous operations
    pub fn invalidate_signature(&mut self) {
        self.raw[510] = 0x00;
        self.raw[511] = 0x00;
    }

    /// Restore the boot sector signature to the valid value
    pub fn restore_signature(&mut self) {
        let sig = Self::VALID_SIGNATURE.to_le_bytes();
        self.raw[510] = sig[0];
        self.raw[511] = sig[1];
    }

    // ===== Calculated Values =====

    /// Get total sectors (prefers 32-bit value)
    pub fn total_sectors(&self) -> u32 {
        let total16 = self.total_sectors_16();
        if total16 == 0 {
            self.total_sectors_32()
        } else {
            total16 as u32
        }
    }

    /// Get FAT size in sectors
    pub fn fat_size(&self) -> u32 {
        let fat16 = self.fat_size_16();
        if fat16 == 0 {
            self.fat_size_32()
        } else {
            fat16 as u32
        }
    }

    /// This boot sector as it would look with a different volume geometry
    ///
    /// Code that needs to reason about a layout other than the current one -
    /// the post-resize layout, or the pre-resize one after the fields have been
    /// updated - should derive a boot sector here and ask it, rather than
    /// recomputing the layout arithmetic. Every such copy is a chance for the
    /// copy and the on-disk truth to disagree.
    pub fn with_geometry(&self, total_sectors: u32, fat_size: u32) -> Self {
        let mut updated = self.clone();
        updated.set_total_sectors_32(total_sectors);
        updated.set_fat_size_32(fat_size);
        updated
    }

    /// First sector of the FAT area
    pub fn first_fat_sector(&self) -> u64 {
        self.reserved_sectors() as u64
    }

    /// First sector of the data area
    pub fn first_data_sector(&self) -> u64 {
        let root_dir_sectors =
            (self.root_entry_count() as u64 * 32).div_ceil(self.bytes_per_sector() as u64);

        self.reserved_sectors() as u64
            + (self.num_fats() as u64 * self.fat_size() as u64)
            + root_dir_sectors
    }

    /// Total data sectors, or `None` if the metadata does not fit in the volume.
    ///
    /// The field values here come straight off the disk and are therefore
    /// attacker-controlled: a crafted image can declare FATs larger than the
    /// whole volume. Subtracting those with bare operators panics in debug
    /// builds and wraps to a huge plausible value in release builds, and
    /// validation's FAT32-vs-FAT16 check then reads that wrapped result - so a
    /// crafted image passed validation. Returning `None` makes the impossible
    /// geometry impossible to mistake for a real one.
    pub fn data_sectors(&self) -> Option<u64> {
        let root_dir_sectors =
            (self.root_entry_count() as u64 * 32).div_ceil(self.bytes_per_sector() as u64);

        let metadata_sectors = (self.num_fats() as u64)
            .checked_mul(self.fat_size() as u64)?
            .checked_add(self.reserved_sectors() as u64)?
            .checked_add(root_dir_sectors)?;

        (self.total_sectors() as u64).checked_sub(metadata_sectors)
    }

    /// Total number of data clusters, or `None` if the geometry cannot express
    /// one - either the metadata does not fit ([`Self::data_sectors`]) or
    /// `sectors_per_cluster` is zero.
    pub fn data_clusters(&self) -> Option<u32> {
        let clusters = self
            .data_sectors()?
            .checked_div(self.sectors_per_cluster() as u64)?;
        u32::try_from(clusters).ok()
    }

    /// Convert cluster number to first sector of that cluster
    pub fn cluster_to_sector(&self, cluster: u32) -> u64 {
        self.first_data_sector() + ((cluster - 2) as u64 * self.sectors_per_cluster() as u64)
    }

    /// Bytes per cluster
    pub fn bytes_per_cluster(&self) -> u32 {
        self.bytes_per_sector() as u32 * self.sectors_per_cluster() as u32
    }
}

impl std::fmt::Debug for BootSector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootSector")
            .field("bytes_per_sector", &self.bytes_per_sector())
            .field("sectors_per_cluster", &self.sectors_per_cluster())
            .field("reserved_sectors", &self.reserved_sectors())
            .field("num_fats", &self.num_fats())
            .field("total_sectors", &self.total_sectors())
            .field("fat_size", &self.fat_size())
            .field("root_cluster", &self.root_cluster())
            .field("fs_info_sector", &self.fs_info_sector())
            .field("backup_boot_sector", &self.backup_boot_sector())
            // Reachable from error paths, so it must render an impossible
            // geometry rather than panic on one.
            .field("data_clusters", &self.data_clusters())
            .field("first_data_sector", &self.first_data_sector())
            .finish()
    }
}

/// FSInfo Sector structure (FAT32 only)
///
/// Contains hints about free cluster count and next free cluster.
/// Supports sector sizes of 512, 1024, 2048, or 4096 bytes.
#[derive(Clone)]
pub struct FSInfo {
    /// Full sector data (variable size)
    raw: Vec<u8>,
}

impl FSInfo {
    /// Lead signature value (offset 0)
    pub const LEAD_SIG: u32 = 0x41615252;
    /// Structure signature value (offset 484)
    pub const STRUC_SIG: u32 = 0x61417272;
    /// Trail signature value (offset 508)
    pub const TRAIL_SIG: u32 = 0xAA550000;
    /// Unknown free count value
    pub const UNKNOWN_FREE: u32 = 0xFFFFFFFF;

    /// Parse FSInfo from raw bytes
    /// The input must be at least 512 bytes and will be stored in full
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < 512 {
            return Err(Error::FSInfoValidation(format!(
                "FSInfo sector too small: {} bytes",
                bytes.len()
            )));
        }

        // Store the full sector data for read-modify-write
        Ok(Self {
            raw: bytes.to_vec(),
        })
    }

    /// Get the raw bytes (full sector)
    pub fn as_bytes(&self) -> &[u8] {
        &self.raw
    }

    /// Get mutable raw bytes
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.raw
    }

    /// Get the sector size this FSInfo was read with
    pub fn sector_size(&self) -> usize {
        self.raw.len()
    }

    /// Lead signature (offset 0, 4 bytes) - must be 0x41615252
    pub fn lead_sig(&self) -> u32 {
        u32::from_le_bytes([self.raw[0], self.raw[1], self.raw[2], self.raw[3]])
    }

    /// Structure signature (offset 484, 4 bytes) - must be 0x61417272
    pub fn struc_sig(&self) -> u32 {
        u32::from_le_bytes([self.raw[484], self.raw[485], self.raw[486], self.raw[487]])
    }

    /// Free cluster count (offset 488, 4 bytes)
    /// 0xFFFFFFFF means unknown
    pub fn free_count(&self) -> u32 {
        u32::from_le_bytes([self.raw[488], self.raw[489], self.raw[490], self.raw[491]])
    }

    /// Set free cluster count
    pub fn set_free_count(&mut self, count: u32) {
        let bytes = count.to_le_bytes();
        self.raw[488..492].copy_from_slice(&bytes);
    }

    /// Next free cluster hint (offset 492, 4 bytes)
    /// 0xFFFFFFFF means unknown
    pub fn next_free(&self) -> u32 {
        u32::from_le_bytes([self.raw[492], self.raw[493], self.raw[494], self.raw[495]])
    }

    /// Set next free cluster hint
    pub fn set_next_free(&mut self, cluster: u32) {
        let bytes = cluster.to_le_bytes();
        self.raw[492..496].copy_from_slice(&bytes);
    }

    /// Trail signature (offset 508, 4 bytes) - must be 0xAA550000
    pub fn trail_sig(&self) -> u32 {
        u32::from_le_bytes([self.raw[508], self.raw[509], self.raw[510], self.raw[511]])
    }
}

impl std::fmt::Debug for FSInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FSInfo")
            .field("lead_sig", &format!("{:#010X}", self.lead_sig()))
            .field("struc_sig", &format!("{:#010X}", self.struc_sig()))
            .field("free_count", &self.free_count())
            .field("next_free", &self.next_free())
            .field("trail_sig", &format!("{:#010X}", self.trail_sig()))
            .finish()
    }
}

/// FAT table entry constants
pub mod fat_entry {
    /// Free cluster
    pub const FREE: u32 = 0x00000000;
    /// Reserved cluster (first valid cluster is 2)
    pub const RESERVED_START: u32 = 0x00000001;
    /// End of chain markers (any value >= this)
    pub const END_OF_CHAIN_MIN: u32 = 0x0FFFFFF8;
    /// End of chain marker (typical value)
    pub const END_OF_CHAIN: u32 = 0x0FFFFFFF;
    /// Bad cluster marker
    pub const BAD_CLUSTER: u32 = 0x0FFFFFF7;
    /// Mask for valid cluster bits (lower 28 bits)
    pub const CLUSTER_MASK: u32 = 0x0FFFFFFF;

    /// Check if a FAT entry is free
    pub fn is_free(entry: u32) -> bool {
        (entry & CLUSTER_MASK) == FREE
    }

    /// Check if a FAT entry marks end of chain
    pub fn is_end_of_chain(entry: u32) -> bool {
        (entry & CLUSTER_MASK) >= END_OF_CHAIN_MIN
    }

    /// Check if a FAT entry marks a bad cluster
    pub fn is_bad(entry: u32) -> bool {
        (entry & CLUSTER_MASK) == BAD_CLUSTER
    }

    /// Check if a FAT entry points to another cluster
    pub fn is_chain(entry: u32) -> bool {
        let masked = entry & CLUSTER_MASK;
        (2..BAD_CLUSTER).contains(&masked)
    }

    /// Get the next cluster number from an entry
    pub fn next_cluster(entry: u32) -> Option<u32> {
        if is_chain(entry) {
            Some(entry & CLUSTER_MASK)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a boot sector with an explicit FAT32 geometry.
    ///
    /// Only the fields the layout accessors read are set; everything else stays
    /// zero, which is what a FAT32 boot sector holds for them anyway
    /// (`root_entry_count`, `total_sectors_16`, `fat_size_16`).
    fn boot_sector_with(
        bytes_per_sector: u16,
        sectors_per_cluster: u8,
        reserved_sectors: u16,
        num_fats: u8,
        total_sectors: u32,
        fat_size: u32,
    ) -> BootSector {
        let mut data = [0u8; 512];
        data[11..13].copy_from_slice(&bytes_per_sector.to_le_bytes());
        data[13] = sectors_per_cluster;
        data[14..16].copy_from_slice(&reserved_sectors.to_le_bytes());
        data[16] = num_fats;
        data[21] = 0xF8;
        data[32..36].copy_from_slice(&total_sectors.to_le_bytes());
        data[36..40].copy_from_slice(&fat_size.to_le_bytes());
        data[44..48].copy_from_slice(&2u32.to_le_bytes());
        data[510] = 0x55;
        data[511] = 0xAA;
        BootSector::from_bytes(&data).unwrap()
    }

    /// Pins the derived layout arithmetic on geometries that are all valid
    /// FAT32. The expected values are worked out by hand in the table rather
    /// than by re-running the formula under test, so a change to how
    /// `data_sectors`/`data_clusters` compute cannot move the good-path answers
    /// without failing here.
    /// One geometry plus the layout values it must produce.
    struct LayoutCase {
        bytes_per_sector: u16,
        sectors_per_cluster: u8,
        reserved_sectors: u16,
        num_fats: u8,
        total_sectors: u32,
        fat_size: u32,
        expect_first_data_sector: u64,
        expect_data_sectors: u64,
        expect_data_clusters: u32,
    }

    #[test]
    fn layout_accessors_on_valid_geometries() {
        let cases = [
            // 32 + 2*7813 = 15_658; 2_000_000 - 15_658 = 1_984_342; /8 = 248_042 rem 6
            LayoutCase {
                bytes_per_sector: 512,
                sectors_per_cluster: 8,
                reserved_sectors: 32,
                num_fats: 2,
                total_sectors: 2_000_000,
                fat_size: 7813,
                expect_first_data_sector: 15_658,
                expect_data_sectors: 1_984_342,
                expect_data_clusters: 248_042,
            },
            // 32 + 2*7813 = 15_658; 1_000_000 - 15_658 = 984_342; /1 = 984_342
            LayoutCase {
                bytes_per_sector: 512,
                sectors_per_cluster: 1,
                reserved_sectors: 32,
                num_fats: 2,
                total_sectors: 1_000_000,
                fat_size: 7813,
                expect_first_data_sector: 15_658,
                expect_data_sectors: 984_342,
                expect_data_clusters: 984_342,
            },
            // 32 + 2*500 = 1_032; 500_000 - 1_032 = 498_968; /1 = 498_968
            LayoutCase {
                bytes_per_sector: 4096,
                sectors_per_cluster: 1,
                reserved_sectors: 32,
                num_fats: 2,
                total_sectors: 500_000,
                fat_size: 500,
                expect_first_data_sector: 1_032,
                expect_data_sectors: 498_968,
                expect_data_clusters: 498_968,
            },
            // 32 + 2*1200 = 2_432; 1_200_000 - 2_432 = 1_197_568; /4 = 299_392
            LayoutCase {
                bytes_per_sector: 1024,
                sectors_per_cluster: 4,
                reserved_sectors: 32,
                num_fats: 2,
                total_sectors: 1_200_000,
                fat_size: 1200,
                expect_first_data_sector: 2_432,
                expect_data_sectors: 1_197_568,
                expect_data_clusters: 299_392,
            },
            // 32 + 1*900 = 932; 800_000 - 932 = 799_068; /2 = 399_534
            LayoutCase {
                bytes_per_sector: 2048,
                sectors_per_cluster: 2,
                reserved_sectors: 32,
                num_fats: 1,
                total_sectors: 800_000,
                fat_size: 900,
                expect_first_data_sector: 932,
                expect_data_sectors: 799_068,
                expect_data_clusters: 399_534,
            },
        ];

        for case in cases {
            let boot = boot_sector_with(
                case.bytes_per_sector,
                case.sectors_per_cluster,
                case.reserved_sectors,
                case.num_fats,
                case.total_sectors,
                case.fat_size,
            );
            let label = format!(
                "bps={} spc={} total={} fat_size={}",
                case.bytes_per_sector, case.sectors_per_cluster, case.total_sectors, case.fat_size
            );
            assert_eq!(
                boot.first_data_sector(),
                case.expect_first_data_sector,
                "first_data_sector {label}"
            );
            assert_eq!(
                boot.data_sectors(),
                Some(case.expect_data_sectors),
                "data_sectors {label}"
            );
            assert_eq!(
                boot.data_clusters(),
                Some(case.expect_data_clusters),
                "data_clusters {label}"
            );
            // Every case must stay a legitimate FAT32 volume, or it is not
            // exercising the good path this test exists to protect.
            assert!(
                case.expect_data_clusters >= 65525,
                "case is not FAT32: {label}"
            );
        }
    }

    /// A boot sector whose FATs do not fit inside the volume has no meaningful
    /// data area. The arithmetic must report that rather than wrapping into a
    /// huge plausible-looking number (release) or panicking (debug).
    ///
    /// Geometry is the report's own [F-R7] reproduction.
    #[test]
    fn data_area_is_unrepresentable_when_the_layout_exceeds_the_volume() {
        let boot = boot_sector_with(512, 8, 32, 2, 2_000_000, 15_728_640);
        assert_eq!(
            boot.data_sectors(),
            None,
            "FATs larger than the volume must not yield a data-sector count"
        );
        assert_eq!(boot.data_clusters(), None);
    }

    /// Boundary: the layout exactly filling the volume leaves zero data sectors,
    /// which is representable - it is one sector further that is not.
    #[test]
    fn data_area_boundary_is_exact() {
        // reserved 32 + 2*984 = 2000 sectors of metadata in a 2000-sector volume.
        let exact = boot_sector_with(512, 1, 32, 2, 2_000, 984);
        assert_eq!(exact.data_sectors(), Some(0));
        assert_eq!(exact.data_clusters(), Some(0));

        let one_too_far = boot_sector_with(512, 1, 32, 2, 1_999, 984);
        assert_eq!(one_too_far.data_sectors(), None);
    }

    /// `sectors_per_cluster` of zero is rejected by validation, but `Debug` and
    /// other callers can still reach a boot sector holding it.
    #[test]
    fn data_clusters_is_unrepresentable_without_a_cluster_size() {
        let boot = boot_sector_with(512, 0, 32, 2, 2_000_000, 7813);
        assert_eq!(boot.data_sectors(), Some(1_984_342));
        assert_eq!(
            boot.data_clusters(),
            None,
            "division by a zero cluster size"
        );
    }

    /// Debug-formatting is reachable from error paths and logging, so it must
    /// survive a boot sector that failed validation.
    #[test]
    fn debug_formatting_survives_a_corrupt_boot_sector() {
        let boot = boot_sector_with(512, 0, 32, 2, 2_000_000, 15_728_640);
        let rendered = format!("{boot:?}");
        assert!(
            rendered.contains("BootSector"),
            "expected a BootSector rendering, got: {rendered}"
        );
    }

    /// `cluster_to_sector` must keep agreeing with `first_data_sector` for the
    /// first and a later data cluster.
    #[test]
    fn cluster_to_sector_is_anchored_to_the_data_area_start() {
        let boot = boot_sector_with(512, 8, 32, 2, 2_000_000, 7813);
        assert_eq!(boot.cluster_to_sector(2), 15_658);
        assert_eq!(boot.cluster_to_sector(3), 15_658 + 8);
        assert_eq!(boot.cluster_to_sector(1000), 15_658 + 998 * 8);
    }

    /// [F-R36]'s safety net: every legitimate cluster must keep mapping to the
    /// same sector once `cluster_to_sector` starts checking its arithmetic.
    ///
    /// The risk a checked conversion introduces is not a wrong answer but a
    /// *refused* one -- rejecting a cluster the filesystem legitimately holds.
    /// So the case that matters is the last valid cluster of each geometry, and
    /// the expectations are spelled out as literals worked out by hand rather
    /// than recomputed from `first_data_sector`, which the fix also touches.
    #[test]
    fn cluster_to_sector_is_exact_from_the_first_to_the_last_valid_cluster() {
        /// One geometry and the cluster→sector answers it must produce.
        struct ClusterCase {
            bytes_per_sector: u16,
            sectors_per_cluster: u8,
            total_sectors: u32,
            fat_size: u32,
            /// Where the data area starts, i.e. cluster 2's first sector.
            first_data_sector: u64,
            /// The highest cluster this geometry can address.
            last_cluster: u32,
            /// That cluster's first sector.
            last_cluster_sector: u64,
        }

        // Reserved sectors and FAT count are 32 and 2 throughout, so the data
        // area starts at 32 + 2 * fat_size.
        const RESERVED: u16 = 32;
        const NUM_FATS: u8 = 2;

        let cases = [
            ClusterCase {
                bytes_per_sector: 512,
                sectors_per_cluster: 8,
                total_sectors: 2_000_000,
                fat_size: 7813,
                first_data_sector: 15_658,
                last_cluster: 248_043,
                last_cluster_sector: 1_999_986,
            },
            ClusterCase {
                bytes_per_sector: 4096,
                sectors_per_cluster: 1,
                total_sectors: 1_000_000,
                fat_size: 1000,
                first_data_sector: 2_032,
                last_cluster: 997_969,
                last_cluster_sector: 999_999,
            },
            ClusterCase {
                bytes_per_sector: 512,
                sectors_per_cluster: 1,
                total_sectors: 70_000,
                fat_size: 550,
                first_data_sector: 1_132,
                last_cluster: 68_869,
                last_cluster_sector: 69_999,
            },
        ];

        for case in cases {
            let bps = case.bytes_per_sector;
            let spc = case.sectors_per_cluster;
            let boot = boot_sector_with(
                bps,
                spc,
                RESERVED,
                NUM_FATS,
                case.total_sectors,
                case.fat_size,
            );

            assert_eq!(
                boot.first_data_sector(),
                case.first_data_sector,
                "first_data_sector for bps={bps} spc={spc}"
            );
            assert_eq!(
                boot.data_clusters(),
                Some(case.last_cluster - 1),
                "data_clusters for bps={bps} spc={spc}"
            );

            assert_eq!(
                boot.cluster_to_sector(2),
                case.first_data_sector,
                "cluster 2 must start the data area for bps={bps} spc={spc}"
            );
            assert_eq!(
                boot.cluster_to_sector(case.last_cluster),
                case.last_cluster_sector,
                "last valid cluster {} for bps={bps} spc={spc}",
                case.last_cluster
            );
            assert!(
                case.last_cluster_sector + spc as u64 <= case.total_sectors as u64,
                "the last cluster must fit inside the volume for bps={bps} spc={spc}"
            );
        }
    }

    #[test]
    fn test_fat_entry_helpers() {
        assert!(fat_entry::is_free(0x00000000));
        assert!(!fat_entry::is_free(0x00000002));

        assert!(fat_entry::is_end_of_chain(0x0FFFFFFF));
        assert!(fat_entry::is_end_of_chain(0x0FFFFFF8));
        assert!(!fat_entry::is_end_of_chain(0x00000002));

        assert!(fat_entry::is_bad(0x0FFFFFF7));
        assert!(!fat_entry::is_bad(0x0FFFFFF8));

        assert!(fat_entry::is_chain(0x00000002));
        assert!(fat_entry::is_chain(0x00001234));
        assert!(!fat_entry::is_chain(0x00000000));
        assert!(!fat_entry::is_chain(0x0FFFFFFF));

        assert_eq!(fat_entry::next_cluster(0x00001234), Some(0x00001234));
        assert_eq!(fat_entry::next_cluster(0xF0001234), Some(0x00001234)); // Upper bits masked
        assert_eq!(fat_entry::next_cluster(0x0FFFFFFF), None);
    }

    #[test]
    fn test_boot_sector_parsing() {
        // Create a minimal valid FAT32 boot sector
        let mut data = [0u8; 512];

        // Jump instruction
        data[0] = 0xEB;
        data[1] = 0x58;
        data[2] = 0x90;

        // OEM name
        data[3..11].copy_from_slice(b"MSDOS5.0");

        // Bytes per sector (512)
        data[11] = 0x00;
        data[12] = 0x02;

        // Sectors per cluster (8)
        data[13] = 0x08;

        // Reserved sectors (32)
        data[14] = 0x20;
        data[15] = 0x00;

        // Number of FATs (2)
        data[16] = 0x02;

        // Total sectors 32 (1000000)
        let total_sectors: u32 = 1_000_000;
        data[32..36].copy_from_slice(&total_sectors.to_le_bytes());

        // FAT size 32 (7813)
        let fat_size: u32 = 7813;
        data[36..40].copy_from_slice(&fat_size.to_le_bytes());

        // Root cluster (2)
        data[44..48].copy_from_slice(&2u32.to_le_bytes());

        // FSInfo sector (1)
        data[48] = 0x01;

        // Backup boot sector (6)
        data[50] = 0x06;

        // Boot signature
        data[510] = 0x55;
        data[511] = 0xAA;

        let boot = BootSector::from_bytes(&data).unwrap();

        assert_eq!(boot.bytes_per_sector(), 512);
        assert_eq!(boot.sectors_per_cluster(), 8);
        assert_eq!(boot.reserved_sectors(), 32);
        assert_eq!(boot.num_fats(), 2);
        assert_eq!(boot.total_sectors(), 1_000_000);
        assert_eq!(boot.fat_size(), 7813);
        assert_eq!(boot.root_cluster(), 2);
        assert_eq!(boot.fs_info_sector(), 1);
        assert_eq!(boot.backup_boot_sector(), 6);
        assert_eq!(boot.boot_signature(), 0xAA55);
    }

    #[test]
    fn test_fsinfo_parsing() {
        let mut data = [0u8; 512];

        // Lead signature
        data[0..4].copy_from_slice(&FSInfo::LEAD_SIG.to_le_bytes());

        // Structure signature
        data[484..488].copy_from_slice(&FSInfo::STRUC_SIG.to_le_bytes());

        // Free count (12345)
        data[488..492].copy_from_slice(&12345u32.to_le_bytes());

        // Next free (100)
        data[492..496].copy_from_slice(&100u32.to_le_bytes());

        // Trail signature
        data[508..512].copy_from_slice(&FSInfo::TRAIL_SIG.to_le_bytes());

        let fsinfo = FSInfo::from_bytes(&data).unwrap();

        assert_eq!(fsinfo.lead_sig(), FSInfo::LEAD_SIG);
        assert_eq!(fsinfo.struc_sig(), FSInfo::STRUC_SIG);
        assert_eq!(fsinfo.free_count(), 12345);
        assert_eq!(fsinfo.next_free(), 100);
        assert_eq!(fsinfo.trail_sig(), FSInfo::TRAIL_SIG);
    }
}
