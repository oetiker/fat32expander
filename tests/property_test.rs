//! [F-13]: properties of a whole resize, over generated filesystems.
//!
//! The images are hand-built, so these run on every `cargo test` and need no
//! dosfstools. Each case writes an image of about 40-60 MB, so the case count
//! is kept small.

use fat32expander::{resize_fat32, ResizeOptions};
use proptest::prelude::*;
use std::collections::BTreeSet;
use tempfile::NamedTempFile;

const BPS: usize = 512;
const RESERVED: usize = 32;
const MIN_FAT32_CLUSTERS: u32 = 65_525;

struct Image {
    file: NamedTempFile,
    clusters: u32,
    fat: Vec<u32>,
    in_use: Vec<u32>,
}

/// The bytes a test puts into `cluster`, distinct per cluster and per case.
fn pattern(cluster: u32, seed: u8) -> Vec<u8> {
    (0..BPS)
        .map(|i| (cluster as usize * 7 + i).wrapping_add(seed as usize) as u8)
        .collect()
}

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}

/// A FAT32 volume with one sector per cluster and two FATs, `growth` sectors
/// smaller than its file. `in_use` clusters form one chain and hold
/// [`pattern`] data; everything else is free.
fn build(extra_clusters: u32, growth: u32, in_use: &BTreeSet<u32>, seed: u8) -> Image {
    let clusters = MIN_FAT32_CLUSTERS + extra_clusters;
    let fat_size = ((clusters as usize + 2) * 4).div_ceil(BPS);
    let total = RESERVED + 2 * fat_size + clusters as usize;
    let mut data = vec![0u8; (total + growth as usize) * BPS];

    let boot = &mut data[0..BPS];
    boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"MSDOS5.0");
    boot[11..13].copy_from_slice(&(BPS as u16).to_le_bytes());
    boot[13] = 1;
    boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    boot[16] = 2;
    boot[21] = 0xF8;
    boot[32..36].copy_from_slice(&(total as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&(fat_size as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48] = 1;
    boot[50] = 6;
    boot[67..71].copy_from_slice(&(0xC0DE_0000u32 | seed as u32).to_le_bytes());
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

    let mut fat = vec![0u32; clusters as usize + 2];
    fat[0] = 0x0FFF_FFF8;
    fat[1] = 0x0FFF_FFFF;
    let in_use: Vec<u32> = in_use.iter().copied().collect();
    for pair in in_use.windows(2) {
        fat[pair[0] as usize] = pair[1];
    }
    fat[*in_use.last().unwrap() as usize] = 0x0FFF_FFFF;
    for copy in 0..2 {
        let at = (RESERVED + copy * fat_size) * BPS;
        for (i, entry) in fat.iter().enumerate() {
            data[at + i * 4..at + i * 4 + 4].copy_from_slice(&entry.to_le_bytes());
        }
    }

    let first_data_sector = RESERVED + 2 * fat_size;
    for &cluster in &in_use {
        let at = (first_data_sector + cluster as usize - 2) * BPS;
        data[at..at + BPS].copy_from_slice(&pattern(cluster, seed));
    }

    let file = NamedTempFile::new().expect("temp file");
    std::fs::write(file.path(), &data).expect("write image");
    Image {
        file,
        clusters,
        fat,
        in_use,
    }
}

fn in_use_clusters() -> impl Strategy<Value = BTreeSet<u32>> {
    // Low clusters are the ones the grown FAT overwrites, so they are the
    // ones that move; a few high ones check that the rest stays put.
    (
        proptest::collection::btree_set(3u32..3000, 0..200),
        proptest::collection::btree_set(3u32..MIN_FAT32_CLUSTERS, 0..20),
    )
        .prop_map(|(low, high)| {
            let mut all: BTreeSet<u32> = low.union(&high).copied().collect();
            all.insert(2); // the root directory
            all
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(8))]

    #[test]
    fn a_resize_keeps_both_fats_identical_and_every_cluster_intact(
        extra_clusters in 0u32..20_000,
        growth in 1u32..120_000,
        in_use in in_use_clusters(),
        seed in any::<u8>(),
    ) {
        let image = build(extra_clusters, growth, &in_use, seed);
        resize_fat32(ResizeOptions::new(image.file.path())).expect("resize must succeed");
        let data = std::fs::read(image.file.path()).expect("read image");

        let total = le32(&data, 32) as usize;
        let fat_size = le32(&data, 36) as usize;
        prop_assert_eq!(total, data.len() / BPS, "the volume fills the device");
        prop_assert_eq!(&data[0..BPS], &data[6 * BPS..7 * BPS], "backup boot sector");

        let fat1 = &data[RESERVED * BPS..(RESERVED + fat_size) * BPS];
        let fat2 = &data[(RESERVED + fat_size) * BPS..(RESERVED + 2 * fat_size) * BPS];
        prop_assert!(fat1 == fat2, "FAT1 and FAT2 differ");

        for (cluster, &want) in image.fat.iter().enumerate() {
            prop_assert_eq!(le32(fat1, cluster * 4), want, "FAT entry {}", cluster);
        }
        for cluster in image.fat.len()..fat_size * BPS / 4 {
            prop_assert_eq!(le32(fat1, cluster * 4), 0, "new FAT entry {} must be free", cluster);
        }

        let first_data_sector = RESERVED + 2 * fat_size;
        for &cluster in &image.in_use {
            let at = (first_data_sector + cluster as usize - 2) * BPS;
            prop_assert!(
                data[at..at + BPS] == pattern(cluster, seed)[..],
                "cluster {} lost its data", cluster
            );
        }

        let new_clusters = (total - first_data_sector) as u32;
        prop_assert!(new_clusters >= image.clusters);
        prop_assert_eq!(
            le32(&data, BPS + 488),
            new_clusters - image.in_use.len() as u32,
            "FSInfo free count"
        );
    }
}

/// The case the property above first found: a FAT sized exactly, a growth of
/// one sector, and the last cluster in use. The Microsoft formula grew the
/// FAT by more than the growth added, the last cluster's new sector lay past
/// the device, and the resize failed halfway, on every rerun too.
#[test]
fn a_one_sector_growth_of_an_exactly_sized_fat_keeps_the_last_cluster() {
    let in_use: BTreeSet<u32> = [2u32, 100_001].into_iter().collect();
    let image = build(100_000 - MIN_FAT32_CLUSTERS, 1, &in_use, 9);

    resize_fat32(ResizeOptions::new(image.file.path())).expect("resize must succeed");

    let data = std::fs::read(image.file.path()).expect("read image");
    let fat_size = le32(&data, 36) as usize;
    let at = (RESERVED + 2 * fat_size + 100_001 - 2) * BPS;
    assert!(data[at..at + BPS] == pattern(100_001, 9)[..]);
}
