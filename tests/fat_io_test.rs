//! Safety net for [F-15] and [F-18]: the FAT as read and as written by a resize.
//!
//! [F-15] moves the FAT writes of a resize next to `read_fat_table` and gives
//! them one layout formula; [F-18] changes how a FAT is read and copied. The
//! property test in `property_test.rs` covers a two-FAT volume only, and
//! `read_fat_table` was tested only through the relocation plan it feeds.
//! These cases pin both directly:
//!
//! * a single-FAT volume, where there is no copy to sync and the data area
//!   starts right after the one FAT, and
//! * `read_fat_table` returning every entry exactly as stored, all 32 bits,
//!   from the copy asked for, across more sectors than one read chunk.
//!
//! The images are hand-built, so these run on every `cargo test`.

use fat32expander::fat32::{open_fat32_device, read_boot_sector, read_fat_table};
use fat32expander::{resize_fat32, ResizeOptions};
use tempfile::NamedTempFile;

const BPS: usize = 512;
const RESERVED: usize = 32;
const MIN_FAT32_CLUSTERS: u32 = 65_525;

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// The bytes a test puts into `cluster`, distinct per cluster.
fn pattern(cluster: u32) -> Vec<u8> {
    (0..BPS)
        .map(|i| (cluster as usize * 13 + i) as u8)
        .collect()
}

struct Image {
    file: NamedTempFile,
    fat_size: usize,
    /// The entries written into FAT copy `n`, by copy.
    fats: Vec<Vec<u32>>,
}

/// A FAT32 volume with one sector per cluster and `num_fats` FATs, `growth`
/// sectors smaller than its file. `fat_for(copy)` gives each copy's entries;
/// the clusters in `in_use` hold [`pattern`] data.
fn build(
    num_fats: usize,
    growth: usize,
    in_use: &[u32],
    fat_for: impl Fn(usize, usize) -> Vec<u32>,
) -> Image {
    let clusters = MIN_FAT32_CLUSTERS + 100;
    let fat_size = ((clusters as usize + 2) * 4).div_ceil(BPS);
    let total = RESERVED + num_fats * fat_size + clusters as usize;
    let mut data = vec![0u8; (total + growth) * BPS];

    let boot = &mut data[0..BPS];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MSDOS5.0");
    boot[11..13].copy_from_slice(&(BPS as u16).to_le_bytes());
    boot[13] = 1;
    boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    boot[16] = num_fats as u8;
    boot[21] = 0xF8;
    boot[32..36].copy_from_slice(&(total as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&(fat_size as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48] = 1;
    boot[50] = 6;
    boot[67..71].copy_from_slice(&0xFA71_0E57u32.to_le_bytes());
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;
    let primary = data[0..BPS].to_vec();
    data[6 * BPS..7 * BPS].copy_from_slice(&primary);

    let fsinfo = &mut data[BPS..2 * BPS];
    fsinfo[0..4].copy_from_slice(b"RRaA");
    fsinfo[484..488].copy_from_slice(b"rrAa");
    fsinfo[488..492].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    fsinfo[492..496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
    fsinfo[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());

    let entries = fat_size * BPS / 4;
    let mut fats = Vec::new();
    for copy in 0..num_fats {
        let fat = fat_for(copy, entries);
        assert_eq!(fat.len(), entries);
        let at = (RESERVED + copy * fat_size) * BPS;
        for (i, entry) in fat.iter().enumerate() {
            data[at + i * 4..at + i * 4 + 4].copy_from_slice(&entry.to_le_bytes());
        }
        fats.push(fat);
    }

    let first_data_sector = RESERVED + num_fats * fat_size;
    for &cluster in in_use {
        let at = (first_data_sector + cluster as usize - 2) * BPS;
        data[at..at + BPS].copy_from_slice(&pattern(cluster));
    }

    let file = NamedTempFile::new_in(env!("CARGO_TARGET_TMPDIR")).expect("temp file");
    std::fs::write(file.path(), &data).expect("write image");
    Image {
        file,
        fat_size,
        fats,
    }
}

/// One chain through `in_use`, as a FAT with `entries` entries.
fn chain_fat(in_use: &[u32], entries: usize) -> Vec<u32> {
    let mut fat = vec![0u32; entries];
    fat[0] = 0x0FFF_FFF8;
    fat[1] = 0x0FFF_FFFF;
    for pair in in_use.windows(2) {
        fat[pair[0] as usize] = pair[1];
    }
    fat[*in_use.last().unwrap() as usize] = 0x0FFF_FFFF;
    fat
}

/// After a FAT-growing resize, every FAT copy sits at its new place, equals the
/// old FAT followed by free entries, and every in-use cluster kept its data.
fn assert_resize_keeps_every_fat_copy(num_fats: usize) {
    // Low clusters lie where the grown FATs land, so they must move.
    let in_use: Vec<u32> = [2u32, 3, 4, 50, 51, 200, 1000, 60_000].to_vec();
    let image = build(num_fats, 200_000, &in_use, |_, n| chain_fat(&in_use, n));

    resize_fat32(ResizeOptions::new(image.file.path())).expect("resize must succeed");
    let data = std::fs::read(image.file.path()).expect("read image");

    assert_eq!(data[16] as usize, num_fats, "number of FATs");
    let fat_size = le32(&data, 36) as usize;
    assert!(
        fat_size > image.fat_size,
        "the case must grow the FAT ({} -> {fat_size})",
        image.fat_size
    );

    let old = &image.fats[0];
    for copy in 0..num_fats {
        let at = (RESERVED + copy * fat_size) * BPS;
        for (i, &want) in old.iter().enumerate() {
            assert_eq!(le32(&data, at + i * 4), want, "FAT {copy} entry {i}");
        }
        for i in old.len()..fat_size * BPS / 4 {
            assert_eq!(le32(&data, at + i * 4), 0, "FAT {copy} new entry {i}");
        }
    }

    let first_data_sector = RESERVED + num_fats * fat_size;
    for &cluster in &in_use {
        let at = (first_data_sector + cluster as usize - 2) * BPS;
        assert!(
            data[at..at + BPS] == pattern(cluster)[..],
            "cluster {cluster} lost its data"
        );
    }
}

#[test]
fn a_resize_of_a_single_fat_volume_keeps_its_fat_and_data() {
    assert_resize_keeps_every_fat_copy(1);
}

#[test]
fn a_resize_of_a_two_fat_volume_keeps_both_fats_and_data() {
    assert_resize_keeps_every_fat_copy(2);
}

/// `read_fat_table` returns all 32 bits of every entry as stored -- the top
/// four are reserved but not masked -- from the copy asked for, over a FAT of
/// more sectors than one read chunk.
#[test]
fn read_fat_table_returns_every_entry_of_the_copy_asked_for() {
    let image = build(2, 0, &[], |copy, n| {
        (0..n as u32)
            .map(|i| i.wrapping_mul(0x9E37_79B9).rotate_left(copy as u32 * 7 + 3))
            .collect()
    });
    assert!(
        image.fat_size > 256,
        "the FAT must span several read chunks"
    );

    let device = open_fat32_device(image.file.path(), false).expect("open");
    let boot = read_boot_sector(&device).expect("boot sector");
    for copy in 0..2u8 {
        let fat = read_fat_table(&device, &boot, copy).expect("read FAT");
        assert_eq!(
            fat.len(),
            image.fats[copy as usize].len(),
            "FAT {copy} length"
        );
        assert!(
            fat == image.fats[copy as usize],
            "FAT {copy} differs from what was written"
        );
    }
}
