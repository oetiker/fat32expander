//! The on-disk resize checkpoint: record format, codec, and the sector I/O
//! that makes a resize resumable after a crash.
//!
//! Split out of `executor.rs` for [F-R42]. The algorithm that *uses* the
//! checkpoint lives in `executor`; everything about what a checkpoint *is*
//! lives here.

use crate::device::Device;
use crate::error::{Error, Result};
use crate::fat32::{read_fsinfo, write_fsinfo, BootSector, FSInfo};

// ===== Crash-Safe Checkpoint Support =====

/// Magic bytes for resize checkpoint identification
const CHECKPOINT_MAGIC: &[u8; 8] = b"FAT32RSZ";

/// Current checkpoint version.
///
/// Version 2 adds the volume record at [`VOLUME_OFFSET`]; it is otherwise
/// version 1, which is still read ([F-16]). Any other version is refused:
/// it comes from a newer build, and replanning its resize would be unsafe.
const CHECKPOINT_VERSION: u8 = 2;

/// The version written by builds before the volume record existed.
const CHECKPOINT_VERSION_1: u8 = 1;

/// Offset of the volume record: volume serial (4) + CRC32 (4), behind the
/// shift-progress record so the version-1 layout stays untouched.
const VOLUME_OFFSET: usize = PROGRESS_OFFSET + PROGRESS_SIZE;

/// volume serial (4) + CRC32 (4)
const VOLUME_SIZE: usize = 8;

/// Magic bytes for the shift-progress record.
///
/// The record is appended *after* the version-1 checkpoint body rather than
/// folded into it, so the first 32 bytes stay byte-for-byte what version 1
/// always was. A build that expects the record and finds none refuses to
/// resume a data shift instead of guessing how far the previous pass got.
/// Builds before version 2 fail open on a version they do not know and take
/// this build's records for "no checkpoint"; that cannot be fixed from here.
/// This build refuses unknown versions instead ([F-16]).
const PROGRESS_MAGIC: &[u8; 4] = b"PRG1";

/// Offset of the shift-progress record within the checkpoint sector
const PROGRESS_OFFSET: usize = 32;

/// magic (4) + next_cluster (4) + CRC32 (4)
const PROGRESS_SIZE: usize = 12;

/// Record shift progress at least once per this many clusters. The effective interval
/// is also capped by the shift distance in clusters - see [`ShiftProgress`].
pub(crate) const PROGRESS_RECORD_EVERY: u32 = 1024;

/// Resize phase values
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ResizePhase {
    /// Resize started, data shift may be in progress. Boot sector still valid.
    Started = 0,
    /// Data copied, entering danger zone. Boot sector invalidated.
    DataCopied = 1,
    /// FAT written, completing metadata. Boot sector about to be restored.
    FatWritten = 2,
}

impl ResizePhase {
    fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::Started),
            1 => Some(Self::DataCopied),
            2 => Some(Self::FatWritten),
            _ => None,
        }
    }
}

/// Checkpoint stored in new space for crash recovery
///
/// This is written to its [`CheckpointSlot`], the device's last sector when
/// the resize started, and allows resuming an interrupted resize operation.
#[derive(Debug, Clone)]
pub struct ResizeCheckpoint {
    /// Current resize phase
    pub phase: ResizePhase,
    /// Original filesystem total sectors (for validation)
    pub old_total_sectors: u32,
    /// Target filesystem total sectors
    pub new_total_sectors: u32,
    /// Original FAT size in sectors
    pub old_fat_size: u32,
    /// New FAT size in sectors
    pub new_fat_size: u32,
    /// Highest cluster still to be moved by the data shift.
    ///
    /// `None` means no progress record was found, which is not the same as
    /// "nothing moved yet": a shift may have run to an unknown point. Only a
    /// recorded `Some` makes a phase-`Started` resume safe.
    pub shift_progress: Option<u32>,
    /// Serial number of the volume the resize started on (boot sector
    /// offset 67). `None` only for a version-1 record, which predates it.
    pub volume_id: Option<u32>,
}

impl ResizeCheckpoint {
    /// Checkpoint size in bytes (without CRC)
    const DATA_SIZE: usize = 8 + 1 + 1 + 2 + 4 + 4 + 4 + 4; // 28 bytes

    /// Create a new checkpoint
    pub fn new(
        phase: ResizePhase,
        volume_id: u32,
        old_total_sectors: u32,
        new_total_sectors: u32,
        old_fat_size: u32,
        new_fat_size: u32,
    ) -> Self {
        Self {
            phase,
            old_total_sectors,
            new_total_sectors,
            old_fat_size,
            new_fat_size,
            shift_progress: None,
            volume_id: Some(volume_id),
        }
    }

    /// Attach a shift-progress record naming the next cluster still to move.
    ///
    /// [F-R44] Test-only. The resize path assigns `shift_progress` directly as
    /// it descends (`executor.rs`), so this builder exists purely to construct
    /// fixtures; `#[cfg(test)]` says so rather than leaving it looking like
    /// production API.
    #[cfg(test)]
    pub fn with_shift_progress(mut self, next_cluster: u32) -> Self {
        self.shift_progress = Some(next_cluster);
        self
    }

    /// Refuse a record that an interrupted resize of the filesystem behind
    /// `boot` cannot have written.
    ///
    /// [F-02]: the resume path takes its whole size calculation from the
    /// record. Every phase leaves the old geometry in the boot sector, since
    /// the commit writes the new one last and clears the record after it, so
    /// a real record's old geometry always equals the boot sector's. A stale
    /// or foreign record behind a valid boot sector used to drive the shift
    /// plan and the FAT extension with another volume's numbers, and could
    /// commit a FAT too small for the volume it described.
    pub fn check_against(&self, boot: &BootSector) -> Result<()> {
        // [F-16]: geometry alone cannot tell a reformatted volume from the
        // one the record was written for.
        if let Some(record) = self.volume_id {
            if record != boot.volume_id() {
                return Err(Error::CheckpointVolumeMismatch {
                    record,
                    boot: boot.volume_id(),
                });
            }
        }
        let matches = self.old_total_sectors == boot.total_sectors()
            && self.old_fat_size == boot.fat_size()
            && self.new_total_sectors > self.old_total_sectors
            && self.new_fat_size >= self.old_fat_size;
        if matches {
            return Ok(());
        }
        Err(Error::CheckpointMismatch {
            old_total_sectors: self.old_total_sectors,
            new_total_sectors: self.new_total_sectors,
            old_fat_size: self.old_fat_size,
            new_fat_size: self.new_fat_size,
            boot_total_sectors: boot.total_sectors(),
            boot_fat_size: boot.fat_size(),
        })
    }

    /// Serialize checkpoint to bytes with specified sector size
    pub fn to_bytes(&self, sector_size: usize) -> Vec<u8> {
        let mut data = vec![0u8; sector_size];

        // Magic (8 bytes)
        data[0..8].copy_from_slice(CHECKPOINT_MAGIC);

        // Version (1 byte). A record without a volume is a version-1 record;
        // so is one in a sector too small to hold the volume record, which no
        // sector size FAT32 permits is.
        let volume_id = self
            .volume_id
            .filter(|_| sector_size >= VOLUME_OFFSET + VOLUME_SIZE);
        data[8] = if volume_id.is_some() {
            CHECKPOINT_VERSION
        } else {
            CHECKPOINT_VERSION_1
        };

        // Phase (1 byte)
        data[9] = self.phase as u8;

        // Padding (2 bytes)
        data[10] = 0;
        data[11] = 0;

        // old_total_sectors (4 bytes)
        data[12..16].copy_from_slice(&self.old_total_sectors.to_le_bytes());

        // new_total_sectors (4 bytes)
        data[16..20].copy_from_slice(&self.new_total_sectors.to_le_bytes());

        // old_fat_size (4 bytes)
        data[20..24].copy_from_slice(&self.old_fat_size.to_le_bytes());

        // new_fat_size (4 bytes)
        data[24..28].copy_from_slice(&self.new_fat_size.to_le_bytes());

        // CRC32 of data (4 bytes at offset 28)
        let crc = crc32fast::hash(&data[0..Self::DATA_SIZE]);
        data[28..32].copy_from_slice(&crc.to_le_bytes());

        // Shift-progress record, appended after the version-1 body so that
        // body's layout and CRC are untouched.
        if let Some(next_cluster) = self.shift_progress {
            if sector_size >= PROGRESS_OFFSET + PROGRESS_SIZE {
                let p = PROGRESS_OFFSET;
                data[p..p + 4].copy_from_slice(PROGRESS_MAGIC);
                data[p + 4..p + 8].copy_from_slice(&next_cluster.to_le_bytes());
                let crc = crc32fast::hash(&data[p..p + 8]);
                data[p + 8..p + 12].copy_from_slice(&crc.to_le_bytes());
            }
        }

        // Volume record, version 2 only.
        if let Some(volume_id) = volume_id {
            let v = VOLUME_OFFSET;
            data[v..v + 4].copy_from_slice(&volume_id.to_le_bytes());
            let crc = crc32fast::hash(&data[v..v + 4]);
            data[v + 4..v + 8].copy_from_slice(&crc.to_le_bytes());
        }

        data
    }

    /// Parse the volume record a version-2 checkpoint must carry.
    fn parse_volume_id(data: &[u8]) -> Result<u32> {
        let v = VOLUME_OFFSET;
        let field = data
            .get(v..v + VOLUME_SIZE)
            .ok_or(Error::CheckpointCorrupted)?;
        let stored = u32::from_le_bytes([field[4], field[5], field[6], field[7]]);
        if stored != crc32fast::hash(&field[0..4]) {
            return Err(Error::CheckpointCorrupted);
        }
        Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
    }

    /// Parse the optional shift-progress record trailing the checkpoint body.
    ///
    /// A record whose magic is absent means "no progress was recorded"; a
    /// record whose magic is present but whose CRC fails is damage, and is
    /// reported rather than silently treated as absent.
    fn parse_shift_progress(data: &[u8]) -> Result<Option<u32>> {
        let p = PROGRESS_OFFSET;
        if data.len() < p + PROGRESS_SIZE {
            return Ok(None);
        }
        if &data[p..p + 4] != PROGRESS_MAGIC {
            return Ok(None);
        }

        let stored = u32::from_le_bytes([data[p + 8], data[p + 9], data[p + 10], data[p + 11]]);
        if stored != crc32fast::hash(&data[p..p + 8]) {
            return Err(Error::CheckpointCorrupted);
        }

        Ok(Some(u32::from_le_bytes([
            data[p + 4],
            data[p + 5],
            data[p + 6],
            data[p + 7],
        ])))
    }

    /// Parse checkpoint from bytes (sector size independent - only first 32 bytes matter)
    pub fn from_bytes(data: &[u8]) -> Result<Option<Self>> {
        // Need at least 32 bytes for checkpoint data
        if data.len() < 32 {
            return Ok(None);
        }

        // Check magic
        if &data[0..8] != CHECKPOINT_MAGIC {
            return Ok(None);
        }

        // Check version. Our magic with an unknown version is a newer
        // build's record, not a foreign sector ([F-16]).
        let version = data[8];
        if version != CHECKPOINT_VERSION && version != CHECKPOINT_VERSION_1 {
            return Err(Error::CheckpointVersionUnsupported(version));
        }

        // Verify CRC
        let stored_crc = u32::from_le_bytes([data[28], data[29], data[30], data[31]]);
        let computed_crc = crc32fast::hash(&data[0..Self::DATA_SIZE]);
        if stored_crc != computed_crc {
            return Err(Error::CheckpointCorrupted);
        }

        // Parse phase
        let Some(phase) = ResizePhase::from_u8(data[9]) else {
            return Err(Error::CheckpointCorrupted);
        };

        // Parse fields
        let old_total_sectors = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let new_total_sectors = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let old_fat_size = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        let new_fat_size = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);

        Ok(Some(Self {
            phase,
            old_total_sectors,
            new_total_sectors,
            old_fat_size,
            new_fat_size,
            shift_progress: Self::parse_shift_progress(data)?,
            volume_id: if version == CHECKPOINT_VERSION_1 {
                None
            } else {
                Some(Self::parse_volume_id(data)?)
            },
        }))
    }
}

/// The sector that holds the checkpoint of one resize, fixed for the whole
/// life of that resize.
///
/// [F-03]: a resize puts its checkpoint at the device's last sector, which
/// the shift never writes to. Recomputing that sector on every run loses the
/// record as soon as the device changes size between a crash and the rerun.
/// The slot is therefore chosen once, when the resize starts, and marked in
/// FSInfo, so a later run follows the mark instead of the device size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CheckpointSlot(u64);

impl CheckpointSlot {
    /// The slot for a resize that starts now: the device's last sector.
    pub(crate) fn at_device_end(device: &Device) -> Self {
        Self(device.total_sectors() - 1)
    }

    /// The slot an interrupted resize marked in FSInfo, or the device's last
    /// sector if none is marked: a resize that crashed before it could mark
    /// one, or a build that predates the mark.
    pub(crate) fn locate(device: &Device, fsinfo: &FSInfo) -> Self {
        match fsinfo.resize_record_sector() {
            Some(sector) => Self(sector),
            None => Self::at_device_end(device),
        }
    }

    pub(crate) fn sector(self) -> u64 {
        self.0
    }

    /// Whether the slot can hold the record of a resize of `boot`.
    ///
    /// A record always sits beyond the filesystem it belongs to. A slot
    /// inside the filesystem is user data: a filesystem that already fills
    /// the device, or a mark a completed resize did not get to clear.
    fn is_beyond(self, boot: &BootSector) -> bool {
        self.0 >= boot.total_sectors() as u64
    }

    /// The device has shrunk past the slot since the resize started.
    fn outside_device(self, device: &Device) -> Result<()> {
        if self.0 >= device.total_sectors() {
            return Err(Error::CheckpointOutsideDevice {
                sector: self.0,
                device_sectors: device.total_sectors(),
            });
        }
        Ok(())
    }
}

/// Write checkpoint to its slot
pub(crate) fn write_checkpoint(
    device: &Device,
    slot: CheckpointSlot,
    checkpoint: &ResizeCheckpoint,
) -> Result<()> {
    let sector_size = device.sector_size() as usize;
    device.write_sector(slot.sector(), &checkpoint.to_bytes(sector_size))?;
    device.sync()?;
    Ok(())
}

/// Record `slot` in the FSInfo sector, so a later run finds the checkpoint
/// even if the device changes size in between ([F-03]).
///
/// Durable on return: the caller moves data only after this, and the mark is
/// what makes that move resumable.
pub(crate) fn mark_checkpoint_slot(
    device: &Device,
    fsinfo_sector: u16,
    slot: CheckpointSlot,
) -> Result<()> {
    let mut fsinfo = read_fsinfo(device, fsinfo_sector)?;
    fsinfo.set_resize_record_sector(Some(slot.sector()));
    write_fsinfo(device, &fsinfo, fsinfo_sector)?;
    device.sync()?;
    Ok(())
}

/// Read the checkpoint from its slot, if the slot can hold one
pub(crate) fn read_checkpoint(
    device: &Device,
    boot: &BootSector,
    slot: CheckpointSlot,
) -> Result<Option<ResizeCheckpoint>> {
    if !slot.is_beyond(boot) {
        return Ok(None);
    }
    slot.outside_device(device)?;
    let data = device.read_sector(slot.sector())?;
    ResizeCheckpoint::from_bytes(&data)
}

/// Clear the checkpoint by zeroing its slot
pub(crate) fn clear_checkpoint(device: &Device, slot: CheckpointSlot) -> Result<()> {
    let zeros = vec![0u8; device.sector_size() as usize];
    device.write_sector(slot.sector(), &zeros)?;
    Ok(())
}

/// Check for incomplete resize operation and return checkpoint if found
pub(crate) fn check_for_incomplete_resize(
    device: &Device,
    boot: &BootSector,
    slot: CheckpointSlot,
) -> Result<Option<ResizeCheckpoint>> {
    if boot.is_signature_valid() {
        // Boot sector valid - check for checkpoint anyway (phase 0 crash)
        return read_checkpoint(device, boot, slot);
    }

    // Boot sector was invalidated - we MUST find a valid checkpoint
    if !slot.is_beyond(boot) {
        return Err(Error::InvalidatedFilesystem);
    }
    match read_checkpoint(device, boot, slot)? {
        Some(cp) => Ok(Some(cp)),
        None => Err(Error::InvalidatedFilesystem),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resize::testutil::{
        temp_device, temp_device_with_sector_size, test_boot_sector,
        test_boot_sector_with_sector_size,
    };

    // ===== Checkpoint safety net =====
    //
    // The checkpoint is the only durable record of an interrupted resize, and
    // its layout is spelled out as bare byte offsets in `to_bytes` and again in
    // `from_bytes`. Nothing exercised either side before these tests, so a
    // mismatch between the two would have corrupted crash recovery silently.
    // Any change to the record - a new field, a version bump - has to keep
    // these passing.

    /// A checkpoint carrying values that are distinct from one another, so a
    /// field written to or read from the wrong offset cannot go unnoticed.
    fn test_checkpoint(phase: ResizePhase) -> ResizeCheckpoint {
        ResizeCheckpoint::new(
            phase,
            0xCAFE_F00D,
            0x1111_2222,
            0x3333_4444,
            0x5555_6666,
            0x7777_8888,
        )
    }

    fn assert_checkpoint_eq(actual: &ResizeCheckpoint, expected: &ResizeCheckpoint) {
        assert_eq!(actual.phase, expected.phase);
        assert_eq!(actual.old_total_sectors, expected.old_total_sectors);
        assert_eq!(actual.new_total_sectors, expected.new_total_sectors);
        assert_eq!(actual.old_fat_size, expected.old_fat_size);
        assert_eq!(actual.new_fat_size, expected.new_fat_size);
    }

    /// Every phase and every field must survive a serialize/parse round trip.
    #[test]
    fn checkpoint_round_trips_every_phase_and_field() {
        for phase in [
            ResizePhase::Started,
            ResizePhase::DataCopied,
            ResizePhase::FatWritten,
        ] {
            let checkpoint = test_checkpoint(phase);
            let bytes = checkpoint.to_bytes(512);

            let parsed = ResizeCheckpoint::from_bytes(&bytes)
                .unwrap_or_else(|e| panic!("phase {:?} failed to parse: {:?}", phase, e))
                .unwrap_or_else(|| panic!("phase {:?} parsed as absent", phase));

            assert_checkpoint_eq(&parsed, &checkpoint);
        }
    }

    /// Field values must land at the offsets the parser reads them from. This
    /// asserts the on-disk layout directly rather than comparing the two
    /// implementations against each other, so a matching pair of wrong offsets
    /// is still caught.
    #[test]
    fn checkpoint_writes_the_documented_byte_layout() {
        let bytes = test_checkpoint(ResizePhase::DataCopied).to_bytes(512);

        assert_eq!(&bytes[0..8], CHECKPOINT_MAGIC);
        assert_eq!(bytes[8], CHECKPOINT_VERSION);
        assert_eq!(bytes[9], ResizePhase::DataCopied as u8);
        assert_eq!(&bytes[10..12], &[0, 0], "padding must stay zeroed");
        assert_eq!(&bytes[12..16], &0x1111_2222u32.to_le_bytes());
        assert_eq!(&bytes[16..20], &0x3333_4444u32.to_le_bytes());
        assert_eq!(&bytes[20..24], &0x5555_6666u32.to_le_bytes());
        assert_eq!(&bytes[24..28], &0x7777_8888u32.to_le_bytes());
        assert_eq!(
            &bytes[28..32],
            &crc32fast::hash(&bytes[0..28]).to_le_bytes()
        );
    }

    /// Safety net for [F-16]. A version-1 record, built byte by byte rather
    /// than by `to_bytes`, as builds up to now leave it after a crash. A
    /// version bump must not strand those: a user who upgrades between the
    /// crash and the rerun still has to get a resume, with and without the
    /// shift-progress tail.
    #[test]
    fn a_version_1_record_from_an_earlier_build_is_still_read() {
        let mut v1 = vec![0u8; 512];
        v1[0..8].copy_from_slice(b"FAT32RSZ");
        v1[8] = 1; // version
        v1[9] = 1; // phase DataCopied
        v1[12..16].copy_from_slice(&0x1111_2222u32.to_le_bytes());
        v1[16..20].copy_from_slice(&0x3333_4444u32.to_le_bytes());
        v1[20..24].copy_from_slice(&0x5555_6666u32.to_le_bytes());
        v1[24..28].copy_from_slice(&0x7777_8888u32.to_le_bytes());
        let crc = crc32fast::hash(&v1[0..28]);
        v1[28..32].copy_from_slice(&crc.to_le_bytes());

        let parsed = ResizeCheckpoint::from_bytes(&v1)
            .expect("a version-1 record must parse")
            .expect("a version-1 record must be found");
        assert_checkpoint_eq(&parsed, &test_checkpoint(ResizePhase::DataCopied));
        assert_eq!(parsed.shift_progress, None);

        let mut with_tail = v1.clone();
        with_tail[32..36].copy_from_slice(b"PRG1");
        with_tail[36..40].copy_from_slice(&0x0000_4321u32.to_le_bytes());
        let crc = crc32fast::hash(&with_tail[32..40]);
        with_tail[40..44].copy_from_slice(&crc.to_le_bytes());

        let parsed = ResizeCheckpoint::from_bytes(&with_tail)
            .expect("a version-1 record with progress must parse")
            .expect("a version-1 record with progress must be found");
        assert_checkpoint_eq(&parsed, &test_checkpoint(ResizePhase::DataCopied));
        assert_eq!(parsed.shift_progress, Some(0x4321));
    }

    /// A corrupted record must be reported, never parsed into a plausible
    /// resume plan. Each data byte is covered, so a field left outside the CRC
    /// range would be caught.
    #[test]
    fn checkpoint_rejects_corruption_in_any_covered_byte() {
        let good = test_checkpoint(ResizePhase::FatWritten).to_bytes(512);

        for offset in 0..ResizeCheckpoint::DATA_SIZE {
            let mut corrupt = good.clone();
            corrupt[offset] ^= 0x01;

            match ResizeCheckpoint::from_bytes(&corrupt) {
                // Magic and version are checked before the CRC; a sector that
                // fails the magic is foreign data, not a damaged checkpoint;
                // our magic with an unknown version is refused ([F-16]).
                Ok(None) if offset < 8 => {}
                Err(Error::CheckpointVersionUnsupported(_)) if offset == 8 => {}
                Err(Error::CheckpointCorrupted) => {}
                other => panic!(
                    "flipping byte {} should have been rejected, got {:?}",
                    offset, other
                ),
            }
        }
    }

    /// A phase byte outside the known range is corruption, not a newer format:
    /// resuming would compare it with `<=` against the real phases and skip
    /// work that never ran.
    #[test]
    fn checkpoint_rejects_an_unknown_phase_byte() {
        let mut bytes = test_checkpoint(ResizePhase::Started).to_bytes(512);
        bytes[9] = 3;
        // Re-stamp the CRC so the phase check is what rejects it, not the CRC.
        let crc = crc32fast::hash(&bytes[0..ResizeCheckpoint::DATA_SIZE]);
        bytes[28..32].copy_from_slice(&crc.to_le_bytes());

        assert!(matches!(
            ResizeCheckpoint::from_bytes(&bytes),
            Err(Error::CheckpointCorrupted)
        ));
    }

    /// A sector that is not one of our checkpoints means "no resize in
    /// progress" - it must not raise an error, or an ordinary resize over
    /// arbitrary trailing data would refuse to run.
    #[test]
    fn checkpoint_reads_foreign_sectors_as_absent() {
        let mut wrong_magic = test_checkpoint(ResizePhase::Started).to_bytes(512);
        wrong_magic[0] = b'X';

        for (label, data) in [
            ("all zeroes", vec![0u8; 512]),
            ("arbitrary data", vec![0xA5u8; 512]),
            ("truncated", vec![0u8; 31]),
            ("wrong magic", wrong_magic),
        ] {
            let parsed = ResizeCheckpoint::from_bytes(&data)
                .unwrap_or_else(|e| panic!("{} returned an error: {:?}", label, e));
            assert!(parsed.is_none(), "{} parsed as a checkpoint", label);
        }
    }

    /// [F-16]: a record carrying our magic but a version this build does not
    /// know was read as "no checkpoint", so behind a valid boot sector a
    /// newer build's half-done resize was replanned from scratch. It is
    /// refused now. (This case used to sit in
    /// `checkpoint_reads_foreign_sectors_as_absent`, asserting the flaw.)
    #[test]
    fn a_checkpoint_of_an_unknown_version_is_refused() {
        for version in [0u8, 3, 0xFF] {
            let mut data = test_checkpoint(ResizePhase::Started).to_bytes(512);
            data[8] = version;
            assert!(
                matches!(
                    ResizeCheckpoint::from_bytes(&data),
                    Err(Error::CheckpointVersionUnsupported(v)) if v == version
                ),
                "version {version} was not refused"
            );
        }
    }

    /// [F-16]: a version-2 record names the volume it belongs to, at offset
    /// 44 behind the progress tail, under a CRC of its own.
    #[test]
    fn a_checkpoint_records_the_volume_it_belongs_to() {
        let checkpoint = test_checkpoint(ResizePhase::DataCopied);
        let bytes = checkpoint.to_bytes(512);
        assert_eq!(bytes[8], 2);
        assert_eq!(&bytes[44..48], &0xCAFE_F00Du32.to_le_bytes());
        assert_eq!(
            &bytes[48..52],
            &crc32fast::hash(&bytes[44..48]).to_le_bytes()
        );
        let parsed = ResizeCheckpoint::from_bytes(&bytes).unwrap().unwrap();
        assert_eq!(parsed.volume_id, Some(0xCAFE_F00D));

        let mut damaged = bytes.clone();
        damaged[45] ^= 0x01;
        assert!(matches!(
            ResizeCheckpoint::from_bytes(&damaged),
            Err(Error::CheckpointCorrupted)
        ));
    }

    /// [F-16]: a record of another volume is refused even when its geometry
    /// matches, as it does after `mkfs.fat` with the same parameters over a
    /// device whose last sector still holds an old record. A version-1 record
    /// has no volume to compare and is judged on its geometry alone.
    #[test]
    fn a_checkpoint_of_another_volume_is_refused() {
        let mut raw = test_boot_sector(2000, 100).as_bytes().to_vec();
        raw[67..71].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let boot = BootSector::from_bytes(&raw).unwrap();
        let record = |volume_id| {
            ResizeCheckpoint::new(ResizePhase::DataCopied, volume_id, 2000, 4000, 100, 104)
        };

        assert!(record(0x1234_5678).check_against(&boot).is_ok());
        assert!(matches!(
            record(0x8765_4321).check_against(&boot),
            Err(Error::CheckpointVolumeMismatch {
                record: 0x8765_4321,
                boot: 0x1234_5678
            })
        ));
        let mut v1 = record(0x8765_4321);
        v1.volume_id = None;
        assert!(v1.check_against(&boot).is_ok());
    }

    /// The record occupies a whole sector but its content is sector-size
    /// independent, so a checkpoint written on one geometry stays readable if
    /// the sector size is re-derived differently on the recovery run.
    #[test]
    fn checkpoint_content_is_independent_of_sector_size() {
        let checkpoint = test_checkpoint(ResizePhase::DataCopied);

        let small = checkpoint.to_bytes(512);
        let large = checkpoint.to_bytes(4096);

        assert_eq!(small.len(), 512);
        assert_eq!(large.len(), 4096);
        // Body, progress tail and volume record ([F-16]) end at byte 52.
        assert_eq!(small[0..52], large[0..52]);
        assert!(
            large[52..].iter().all(|&b| b == 0),
            "trailing sector space must stay zeroed"
        );

        assert_checkpoint_eq(
            &ResizeCheckpoint::from_bytes(&large).unwrap().unwrap(),
            &checkpoint,
        );
    }

    /// `Started <= DataCopied <= FatWritten` is what the resume branches in
    /// `resize_fat32` compare against to decide which work to skip. Reordering
    /// the variants would silently re-run or skip a phase.
    #[test]
    fn resize_phases_order_from_earliest_to_latest() {
        assert!(ResizePhase::Started < ResizePhase::DataCopied);
        assert!(ResizePhase::DataCopied < ResizePhase::FatWritten);

        assert_eq!(ResizePhase::from_u8(0), Some(ResizePhase::Started));
        assert_eq!(ResizePhase::from_u8(1), Some(ResizePhase::DataCopied));
        assert_eq!(ResizePhase::from_u8(2), Some(ResizePhase::FatWritten));
        assert_eq!(ResizePhase::from_u8(3), None);
        assert_eq!(ResizePhase::from_u8(255), None);
    }

    /// The progress record has to survive the same round trip as the rest.
    #[test]
    fn checkpoint_round_trips_a_shift_progress_record() {
        let checkpoint = test_checkpoint(ResizePhase::Started).with_shift_progress(4321);
        let parsed = ResizeCheckpoint::from_bytes(&checkpoint.to_bytes(512))
            .unwrap()
            .unwrap();

        assert_checkpoint_eq(&parsed, &checkpoint);
        assert_eq!(parsed.shift_progress, Some(4321));
    }

    /// Appending the progress record must not disturb the version-1 body. A
    /// build that predates the record has to keep reading these checkpoints,
    /// which is the whole reason the record lives past byte 32 instead of
    /// inside the body.
    #[test]
    fn progress_record_leaves_the_version_1_body_untouched() {
        let plain = test_checkpoint(ResizePhase::FatWritten);
        let with_progress = plain.clone().with_shift_progress(4321);

        assert_eq!(
            plain.to_bytes(512)[0..32],
            with_progress.to_bytes(512)[0..32],
            "the version-1 body must be byte-identical with and without progress"
        );
    }

    /// A checkpoint carrying no progress record must report `None` rather than
    /// a plausible cluster number. `None` is what makes the executor refuse a
    /// phase-Started resume instead of guessing where the shift stopped.
    #[test]
    fn checkpoint_without_a_progress_record_reports_none() {
        let bytes = test_checkpoint(ResizePhase::Started).to_bytes(512);

        assert_eq!(&bytes[32..44], &[0u8; 12], "no record should be written");
        assert_eq!(
            ResizeCheckpoint::from_bytes(&bytes)
                .unwrap()
                .unwrap()
                .shift_progress,
            None
        );
    }

    /// A damaged progress record is corruption, not absence. Reading it as
    /// absent would be safe; reading it as a wrong cluster number would not,
    /// so the CRC has to be enforced.
    #[test]
    fn checkpoint_rejects_a_corrupted_progress_record() {
        let good = test_checkpoint(ResizePhase::Started)
            .with_shift_progress(4321)
            .to_bytes(512);

        for offset in PROGRESS_OFFSET + 4..PROGRESS_OFFSET + PROGRESS_SIZE {
            let mut corrupt = good.clone();
            corrupt[offset] ^= 0x01;

            assert!(
                matches!(
                    ResizeCheckpoint::from_bytes(&corrupt),
                    Err(Error::CheckpointCorrupted)
                ),
                "flipping byte {} of the progress record should have been rejected",
                offset
            );
        }
    }

    /// A sector too small to hold the record still yields a usable checkpoint;
    /// the progress simply cannot be stored, and recovery treats that as
    /// unknown rather than failing outright.
    #[test]
    fn progress_record_is_omitted_when_the_sector_cannot_hold_it() {
        let bytes = test_checkpoint(ResizePhase::Started)
            .with_shift_progress(4321)
            .to_bytes(40);

        assert_eq!(bytes.len(), 40);
        assert_eq!(
            ResizeCheckpoint::from_bytes(&bytes)
                .unwrap()
                .unwrap()
                .shift_progress,
            None
        );
    }

    /// The checkpoint lives in the device's last sector - deliberately, so
    /// forward-shifted cluster data starting at `old_total_sectors` cannot
    /// overwrite it. Writing it anywhere else would put it in the path of the
    /// data shift.
    #[test]
    fn checkpoint_round_trips_through_the_devices_last_sector() {
        let (_file, device) = temp_device(4000);
        // Filesystem still describes the pre-grow size, so the checkpoint
        // sector sits in the newly added space.
        let boot = test_boot_sector(2000, 100);
        let checkpoint = test_checkpoint(ResizePhase::DataCopied);

        assert!(
            read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .is_none()
        );

        write_checkpoint(&device, CheckpointSlot::at_device_end(&device), &checkpoint).unwrap();

        let last = device.total_sectors() - 1;
        assert_eq!(
            &device.read_sector(last).unwrap()[0..8],
            CHECKPOINT_MAGIC,
            "checkpoint must be written to the last sector"
        );

        assert_checkpoint_eq(
            &read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .unwrap(),
            &checkpoint,
        );

        clear_checkpoint(&device, CheckpointSlot::at_device_end(&device)).unwrap();
        assert!(
            read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .is_none()
        );
    }

    /// [F-R23]'s cited consequence: `write_checkpoint`, `read_checkpoint` and
    /// `clear_checkpoint` all address `device.total_sectors() - 1`, so the
    /// checkpoint's location depends on the device's sector size being right.
    /// On a 4096-byte-sector filesystem opened with the hardcoded 512 default,
    /// that sector index is eight times too large.
    ///
    /// Whatever establishes the geometry, the checkpoint must land in the last
    /// sector of the device *as the filesystem measures sectors*, and must
    /// round-trip from there. [F-R28]'s bounds check must not reject it.
    #[test]
    fn checkpoint_round_trips_at_a_non_default_sector_size() {
        let (file, device) = temp_device_with_sector_size(4000, 4096);
        let boot = test_boot_sector_with_sector_size(2000, 100, 4096);
        let checkpoint = test_checkpoint(ResizePhase::FatWritten);

        assert_eq!(device.sector_size(), 4096);
        assert_eq!(device.total_sectors(), 4000);

        assert!(
            read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .is_none()
        );

        write_checkpoint(&device, CheckpointSlot::at_device_end(&device), &checkpoint).unwrap();

        // The record must be at byte 3999 * 4096, not 3999 * 512.
        let expected_offset = 3999u64 * 4096;
        assert_eq!(
            &device.read_bytes_at(expected_offset, 8).unwrap()[..],
            CHECKPOINT_MAGIC,
            "checkpoint must land in the last 4096-byte sector"
        );

        assert_checkpoint_eq(
            &read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .unwrap(),
            &checkpoint,
        );

        // Writing it must not have grown the device.
        assert_eq!(
            std::fs::metadata(file.path()).unwrap().len(),
            4000 * 4096,
            "the checkpoint write must stay inside the device"
        );

        clear_checkpoint(&device, CheckpointSlot::at_device_end(&device)).unwrap();
        assert!(
            read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .is_none()
        );
    }

    /// [F-02]: only a record whose old geometry is the boot sector's, and
    /// whose new geometry grows it, can come from an interrupted resize of
    /// that filesystem.
    #[test]
    fn checkpoint_must_describe_the_filesystem_behind_the_boot_sector() {
        let boot = test_boot_sector(2000, 100);
        let record = |old_total, new_total, old_fat, new_fat| {
            ResizeCheckpoint::new(
                ResizePhase::DataCopied,
                boot.volume_id(),
                old_total,
                new_total,
                old_fat,
                new_fat,
            )
            .check_against(&boot)
        };

        assert!(record(2000, 4000, 100, 104).is_ok(), "FAT growth");
        assert!(record(2000, 4000, 100, 100).is_ok(), "no FAT growth");

        for (old_total, new_total, old_fat, new_fat, what) in [
            (1999, 4000, 100, 104, "old size differs"),
            (2000, 4000, 99, 104, "old FAT size differs"),
            (2000, 2000, 100, 104, "volume does not grow"),
            (2000, 1000, 100, 104, "volume shrinks"),
            (2000, 4000, 100, 96, "FAT shrinks"),
        ] {
            assert!(
                matches!(
                    record(old_total, new_total, old_fat, new_fat),
                    Err(Error::CheckpointMismatch { .. })
                ),
                "{what}: the record must be refused"
            );
        }
    }

    /// Crash detection reads the same last sector. It must find a checkpoint
    /// written at a non-default sector size, or an interrupted resize of a
    /// 4096-byte-sector filesystem is unrecoverable.
    #[test]
    fn crash_detection_finds_a_checkpoint_at_a_non_default_sector_size() {
        let (_file, device) = temp_device_with_sector_size(4000, 4096);
        let boot = test_boot_sector_with_sector_size(2000, 100, 4096);
        let checkpoint = test_checkpoint(ResizePhase::DataCopied);

        write_checkpoint(&device, CheckpointSlot::at_device_end(&device), &checkpoint).unwrap();

        let found =
            check_for_incomplete_resize(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .expect("an interrupted resize must be detected");
        assert_checkpoint_eq(&found, &checkpoint);
    }

    /// Once the filesystem covers the whole device there is no room for a
    /// checkpoint, so recovery must report none rather than reading whatever
    /// user data occupies the last sector.
    #[test]
    fn checkpoint_is_not_read_once_the_filesystem_fills_the_device() {
        let (_file, device) = temp_device(2000);

        // Plant a valid checkpoint in the last sector.
        write_checkpoint(
            &device,
            CheckpointSlot::at_device_end(&device),
            &test_checkpoint(ResizePhase::Started),
        )
        .unwrap();

        let boot = test_boot_sector(2000, 100);
        assert!(
            read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .is_none()
        );
    }

    /// An invalidated boot sector means the resize was interrupted inside the
    /// danger zone. Recovery is only possible with a checkpoint, so its absence
    /// must surface as `InvalidatedFilesystem` rather than as "nothing to do".
    #[test]
    fn incomplete_resize_needs_a_checkpoint_when_the_signature_is_invalid() {
        let (_file, device) = temp_device(4000);
        let mut boot = test_boot_sector(2000, 100);
        boot.invalidate_signature();

        assert!(matches!(
            check_for_incomplete_resize(&device, &boot, CheckpointSlot::at_device_end(&device)),
            Err(Error::InvalidatedFilesystem)
        ));

        let checkpoint = test_checkpoint(ResizePhase::DataCopied);
        write_checkpoint(&device, CheckpointSlot::at_device_end(&device), &checkpoint).unwrap();

        assert_checkpoint_eq(
            &check_for_incomplete_resize(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .unwrap(),
            &checkpoint,
        );
    }

    /// A valid signature with a checkpoint present is the phase-0 crash case:
    /// the data shift may have been interrupted before anything was
    /// invalidated, and recovery must still find the record.
    #[test]
    fn incomplete_resize_finds_a_checkpoint_behind_a_valid_signature() {
        let (_file, device) = temp_device(4000);
        let boot = test_boot_sector(2000, 100);

        assert!(check_for_incomplete_resize(
            &device,
            &boot,
            CheckpointSlot::at_device_end(&device)
        )
        .unwrap()
        .is_none());

        let checkpoint = test_checkpoint(ResizePhase::Started);
        write_checkpoint(&device, CheckpointSlot::at_device_end(&device), &checkpoint).unwrap();

        assert_checkpoint_eq(
            &check_for_incomplete_resize(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .unwrap(),
            &checkpoint,
        );
    }

    // ===== [F-03]: the slot is fixed when the resize starts =====

    fn fsinfo_marking(sector: Option<u64>) -> FSInfo {
        let mut data = vec![0u8; 512];
        data[0..4].copy_from_slice(&FSInfo::LEAD_SIG.to_le_bytes());
        data[484..488].copy_from_slice(&FSInfo::STRUC_SIG.to_le_bytes());
        data[508..512].copy_from_slice(&FSInfo::TRAIL_SIG.to_le_bytes());
        let mut fsinfo = FSInfo::from_bytes(&data).unwrap();
        fsinfo.set_resize_record_sector(sector);
        fsinfo
    }

    #[test]
    fn the_fsinfo_mark_round_trips_and_clears() {
        let mut fsinfo = fsinfo_marking(None);
        assert_eq!(fsinfo.resize_record_sector(), None);

        fsinfo.set_resize_record_sector(Some(0x1_2345_6789));
        assert_eq!(fsinfo.resize_record_sector(), Some(0x1_2345_6789));
        assert_eq!(fsinfo.trail_sig(), FSInfo::TRAIL_SIG, "mark overran");

        fsinfo.set_resize_record_sector(None);
        assert_eq!(fsinfo.resize_record_sector(), None);
        assert!(fsinfo.as_bytes()[496..508].iter().all(|&b| b == 0));
    }

    #[test]
    fn a_marked_slot_is_found_after_the_device_grew() {
        // The resize started on a 3000-sector device; it now has 4000.
        let (_file, device) = temp_device(4000);
        let boot = test_boot_sector(2000, 100);
        let slot = CheckpointSlot::locate(&device, &fsinfo_marking(Some(2999)));
        assert_eq!(slot.sector(), 2999);

        let checkpoint = test_checkpoint(ResizePhase::Started);
        write_checkpoint(&device, slot, &checkpoint).unwrap();

        assert_checkpoint_eq(
            &check_for_incomplete_resize(&device, &boot, slot)
                .unwrap()
                .unwrap(),
            &checkpoint,
        );
        // The device's last sector, where an unmarked run would look, has
        // nothing.
        assert!(
            read_checkpoint(&device, &boot, CheckpointSlot::at_device_end(&device))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn an_unmarked_fsinfo_falls_back_to_the_device_end() {
        let (_file, device) = temp_device(4000);
        let slot = CheckpointSlot::locate(&device, &fsinfo_marking(None));
        assert_eq!(slot, CheckpointSlot::at_device_end(&device));
    }

    #[test]
    fn a_mark_inside_the_filesystem_is_not_a_record() {
        // A completed resize that crashed before clearing its mark: the
        // marked sector now holds user data of the grown filesystem.
        let (_file, device) = temp_device(4000);
        let boot = test_boot_sector(4000, 100);
        let slot = CheckpointSlot::locate(&device, &fsinfo_marking(Some(2999)));
        write_checkpoint(&device, slot, &test_checkpoint(ResizePhase::Started)).unwrap();

        assert!(check_for_incomplete_resize(&device, &boot, slot)
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_mark_beyond_a_shrunk_device_needs_recovery() {
        let (_file, device) = temp_device(2500);
        let boot = test_boot_sector(2000, 100);
        let slot = CheckpointSlot::locate(&device, &fsinfo_marking(Some(2999)));

        assert!(matches!(
            check_for_incomplete_resize(&device, &boot, slot),
            Err(Error::CheckpointOutsideDevice {
                sector: 2999,
                device_sectors: 2500
            })
        ));
        let mut invalidated = boot.clone();
        invalidated.invalidate_signature();
        assert!(matches!(
            check_for_incomplete_resize(&device, &invalidated, slot),
            Err(Error::CheckpointOutsideDevice { .. })
        ));
    }
}
