//! Reads `fixtures/vmfs6/fixture-big-v1`: a 17 TiB datastore made by ESXi 7.0.3, of which only the parts that these
//! tests read were captured (92 MiB).
//!
//! Above 16 TiB the logical volume has a second extent, behind a 16 MiB gap on the device. Readers that compute
//! device offsets with a fixed formula return data from 16 MiB too early there. The datastore was filled with four
//! lazily allocated disks of 4100 GiB, so the two test disks lie above the 16 TiB mark.

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

use openvmfs::{BlockSource, ExtentKind, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, sha256_of, ExpectedDisk, PartialSource};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;
const TIB: u64 = 1 << 40;

fn open() -> (Arc<PartialSource>, Volume) {
    let device = Arc::new(own_partial_image("fixture-big-v1"));
    let volume = Volume::open(device.clone()).expect("open fixture");
    (device, volume)
}

fn truth(file: &str) -> String {
    std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-big-v1/truth").join(file)).unwrap()
}

fn esxi_sha256(path: &str) -> String {
    truth("sha256.txt")
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

/// The data extents ESXi listed with `vmkfstools -t0`, as (file offset, length, device offset).
fn esxi_data_extents(name: &str) -> Vec<(u64, u64, u64)> {
    let mut extents = Vec::new();
    for line in truth(&format!("{name}.t0.txt")).lines() {
        // [ offset: length] --> [VMFS -- LVID:.../1:( start --> end)]
        let Some((file, device)) = line.split_once("--> [VMFS -- ") else {
            continue;
        };
        let numbers = |text: &str| -> Vec<u64> {
            text.split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .map(|n| n.parse().unwrap())
                .collect()
        };
        let file = numbers(file);
        let device = numbers(device.rsplit_once(":(").unwrap().1);
        extents.push((file[0], file[1], device[0]));
    }
    extents
}

#[test]
fn the_volume_is_17_tib_and_has_two_extents() {
    let (_, volume) = open();
    let info = volume.info();
    assert_eq!(info.label, "b17");
    assert_eq!(info.capacity, 17 * TIB - 256 * MIB);
    // Below the 16 TiB mark a volume offset is 17 MiB behind its partition offset, above it 33 MiB.
    let partition = info.partition_offset;
    assert_eq!(volume.device_offset(0).unwrap(), partition + 17 * MIB);
    assert_eq!(
        volume.device_offset(16 * TIB - 1).unwrap(),
        partition + 17 * MIB + 16 * TIB - 1
    );
    assert_eq!(volume.device_offset(16 * TIB).unwrap(), partition + 33 * MIB + 16 * TIB);
}

#[test]
fn disks_above_16_tib_read_like_on_esxi() {
    let (_, volume) = open();
    for name in ["thin", "lazy"] {
        let disk = volume.open_disk(&format!("t/{name}.vmdk")).unwrap();
        assert_eq!(sha256_of(&disk), esxi_sha256(&format!("t/{name}-flat.vmdk")), "{name}");
    }
}

#[test]
fn every_written_sector_is_where_it_was_written() {
    let (_, volume) = open();
    // What `fixtures/recipes/pattern.py write` was asked to write, as (first sector, count).
    let thin = ExpectedDisk {
        disk_id: 50,
        generation: 1,
        size: GIB,
        written: vec![(0, 2048), (4096, 2048), (1048576, 4096), (2095104, 2048)],
    };
    let hash = thin.assert_matches(&volume.open_disk("t/thin.vmdk").unwrap(), "thin");
    assert_eq!(hash, esxi_sha256("t/thin-flat.vmdk"));
    let lazy = ExpectedDisk {
        disk_id: 51,
        generation: 1,
        size: 2 * GIB,
        written: vec![(0, 2048), (1046528, 4096), (4192256, 2048)],
    };
    let hash = lazy.assert_matches(&volume.open_disk("t/lazy.vmdk").unwrap(), "lazy");
    assert_eq!(hash, esxi_sha256("t/lazy-flat.vmdk"));
}

#[test]
fn the_block_maps_agree_with_esxi_and_lie_above_16_tib() {
    let (device, volume) = open();
    for name in ["thin", "lazy"] {
        let disk = volume.open_disk(&format!("t/{name}.vmdk")).unwrap();
        let ours: Vec<(u64, u64, u64)> = disk
            .extents()
            .iter()
            .filter_map(|extent| match extent.kind {
                ExtentKind::Data { volume_offset } => {
                    Some((extent.offset, extent.len, volume.device_offset(volume_offset).unwrap()))
                }
                _ => None,
            })
            .collect();
        let esxi = esxi_data_extents(name);
        assert!(!esxi.is_empty());
        assert_eq!(ours, esxi, "{name}");
        for (_, len, device_offset) in ours {
            assert!(
                device_offset > 16 * TIB,
                "{name}: the test disk is meant to lie above 16 TiB"
            );
            assert!(device.covers(device_offset, len));
        }
    }
}

#[test]
fn a_fixed_offset_formula_would_miss_by_16_mib() {
    let (device, volume) = open();
    let disk = volume.open_disk("t/thin.vmdk").unwrap();
    let ExtentKind::Data { volume_offset } = disk.extents()[0].kind else {
        panic!("first block is data")
    };
    let mut right = [0u8; 512];
    disk.read_exact_at(0, &mut right).unwrap();
    assert!(right.starts_with(openvmfs_testkit::PATTERN_MAGIC));
    // What the older tools compute: partition + 17 MiB + volume offset.
    let naive = volume.info().partition_offset + 17 * MIB + volume_offset;
    assert_eq!(volume.device_offset(volume_offset).unwrap(), naive + 16 * MIB);
    let mut wrong = [0u8; 512];
    // Where that place was captured at all, it holds something else.
    if device.read_exact_at(naive, &mut wrong).is_ok() {
        assert_ne!(wrong, right);
    }
}
