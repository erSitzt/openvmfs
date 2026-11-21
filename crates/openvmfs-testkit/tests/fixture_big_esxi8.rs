//! Reads `fixtures/vmfs6/fixture-big-esxi8-v1`: a 40 TiB datastore made by ESXi 8.0 U3, of which only the parts
//! that these tests read were captured (105 MiB).
//!
//! The logical volume of a datastore has a new extent every 16 TiB, each behind a 16 MiB gap on the device. This
//! datastore has three. It was filled with eight lazily allocated disks of 4100 GiB in two rounds, with test disks
//! created after each round, so that test disks lie behind the first gap and behind the second.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use std::sync::Arc;

use openvmfs::{Backing, BlockSource, ExtentKind, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, sha256_of, ExpectedDisk, PartialSource};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const TIB: u64 = 1 << 40;

fn open() -> (Arc<PartialSource>, Volume) {
    let device = Arc::new(own_partial_image("fixture-big-esxi8-v1"));
    let volume = Volume::open(device.clone()).expect("open fixture");
    (device, volume)
}

fn esxi_sha256(path: &str) -> String {
    std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-big-esxi8-v1/truth/sha256.txt"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

/// First volume offset of a disk's data.
fn first_data(volume: &Volume, path: &str) -> u64 {
    let disk = volume.open_disk(path).unwrap();
    disk.extents()
        .iter()
        .find_map(|extent| match extent.kind {
            ExtentKind::Data { volume_offset } => Some(volume_offset),
            _ => None,
        })
        .unwrap()
}

#[test]
fn the_volume_has_three_extents() {
    let (_, volume) = open();
    let info = volume.info();
    assert_eq!(info.label, "fb8");
    assert_eq!(info.version, "6.82");
    assert_eq!(info.capacity, 40 * TIB - 256 * MIB);
    // A volume offset is 17 MiB behind its partition offset, and 16 MiB more behind every 16 TiB mark.
    let partition = info.partition_offset;
    assert_eq!(volume.device_offset(0).unwrap(), partition + 17 * MIB);
    assert_eq!(volume.device_offset(16 * TIB).unwrap(), partition + 33 * MIB + 16 * TIB);
    assert_eq!(
        volume.device_offset(32 * TIB - 1).unwrap(),
        partition + 33 * MIB + 32 * TIB - 1
    );
    assert_eq!(volume.device_offset(32 * TIB).unwrap(), partition + 49 * MIB + 32 * TIB);
}

#[test]
fn disks_behind_both_gaps_read_like_on_esxi() {
    let (_, volume) = open();
    // Where ESXi put them: one lazy disk in the second extent, everything else in the third.
    assert!((16 * TIB..32 * TIB).contains(&first_data(&volume, "t1/lazy.vmdk")));
    for path in ["t1/thin.vmdk", "t2/thin.vmdk", "t2/lazy.vmdk"] {
        assert!(first_data(&volume, path) >= 32 * TIB, "{path}");
    }
    for name in ["t1/thin", "t1/lazy", "t2/thin", "t2/lazy"] {
        let disk = volume.open_disk(&format!("{name}.vmdk")).unwrap();
        assert_eq!(sha256_of(&disk), esxi_sha256(&format!("{name}-flat.vmdk")), "{name}");
    }
}

#[test]
fn every_written_sector_is_where_it_was_written() {
    let (_, volume) = open();
    // What `fixtures/recipes/pattern.py write` was asked to write, as (first sector, count).
    for (stage, id) in [(1u32, 41u32), (2, 43)] {
        let thin = ExpectedDisk {
            disk_id: id,
            generation: 1,
            size: GIB,
            written: vec![(0, 2048), (10240, 2048), (614400, 4096), (2095104, 2048)],
        };
        thin.assert_matches(&volume.open_disk(&format!("t{stage}/thin.vmdk")).unwrap(), "thin");
        let lazy = ExpectedDisk {
            disk_id: id + 1,
            generation: 1,
            size: 2 * GIB,
            written: vec![(0, 8), (204800, 2048), (4194296, 8)],
        };
        lazy.assert_matches(&volume.open_disk(&format!("t{stage}/lazy.vmdk")).unwrap(), "lazy");
    }
}

#[test]
fn the_device_map_points_at_the_same_bytes() {
    let (device, volume) = open();
    for name in ["t1/lazy", "t2/thin"] {
        let disk = volume.open_disk(&format!("{name}.vmdk")).unwrap();
        for range in disk.map(0..disk.capacity(), 4096).unwrap() {
            let Backing::Device { device: index, offset } = range.backing else {
                continue;
            };
            assert_eq!(index, 0);
            let (mut through, mut direct) = (vec![0u8; range.len as usize], vec![0u8; range.len as usize]);
            disk.read_exact_at(range.offset, &mut through).unwrap();
            device.read_exact_at(offset, &mut direct).unwrap();
            assert!(through == direct, "{name} at {:#x}", range.offset);
        }
    }
}

#[test]
fn a_4100_gib_thick_disk_opens() {
    let (_, volume) = open();
    for name in ["fill/s1-f1.vmdk", "fill/s2-f4.vmdk"] {
        let disk = volume.open_disk(name).unwrap();
        assert_eq!(disk.capacity(), 4100 * GIB);
        // Never written: nothing to read, although all of it is allocated.
        assert!(disk.data_ranges().is_empty(), "{name}");
    }
}
