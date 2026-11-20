//! Reads `fixtures/vmfs6/fixture-many-esxi8-v1`: a 48 GiB datastore on which ESXi 8.0 U3 created 100,697 files of
//! 8 KiB. That many files make ESXi grow its sub-block pool past 256 clusters, where the older tools read sub-block
//! addresses in two different ways. The partial image holds what a handful of files need; all 100,697 were compared
//! once on the full image.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;

use openvmfs::fs::AddressingMode;
use openvmfs::Volume;
use openvmfs_testkit::{own_partial_image, sha256_hex};

/// A sub-block cluster: 256 sub-blocks of 64 KiB.
const CLUSTER: u64 = 256 * (64 << 10);
const FILES_PER_DIR: u64 = 1000;

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-many-esxi8-v1"))).expect("open fixture")
}

/// What `fixtures/recipes/make_many.py` wrote into file `index` of directory `dir`.
fn expected(dir: u64, index: u64) -> String {
    sha256_hex(format!("openvmfs-many-{}", dir * FILES_PER_DIR + index).as_bytes())
}

fn check(volume: &Volume, dir: u64, index: u64, addressing: AddressingMode) -> u64 {
    let path = format!("many/d{dir:03}/f{index:04}");
    let file = volume.open_file(&path).unwrap();
    assert_eq!(file.stat().addressing, addressing, "{path}");
    let content = file.read_all(1 << 20).unwrap();
    assert_eq!(content.len(), 8192, "{path}");
    let want = expected(dir, index);
    for chunk in content.chunks(32) {
        let hex: String = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, want, "{path}");
    }
    match file.extents()[0].kind {
        openvmfs::ExtentKind::Data { volume_offset } => volume_offset,
        other => panic!("{path}: {other:?}"),
    }
}

#[test]
fn files_in_sub_block_clusters_above_255_read_as_written() {
    let volume = volume();
    // The first file ever written, in the first sub-block cluster.
    check(&volume, 0, 0, AddressingMode::SubBlocks);
    // Created one after the other: the first sub-block of cluster 255 (address 0x3fc2) and the first of cluster
    // 256 (address 0x4002). The clusters of one group lie back to back on the volume.
    let in_255 = check(&volume, 98, 255, AddressingMode::SubBlocks);
    let in_256 = check(&volume, 98, 256, AddressingMode::SubBlocks);
    assert_eq!(in_256, in_255 + CLUSTER);
    // Further into the clusters above 255.
    check(&volume, 99, 500, AddressingMode::SubBlocks);
}

#[test]
fn files_made_while_no_sub_block_was_free_use_file_blocks() {
    let volume = volume();
    for (dir, index) in [(71, 999), (98, 999), (100, 696)] {
        let offset = check(&volume, dir, index, AddressingMode::FileBlocks);
        assert_eq!(offset % (1 << 20), 0);
    }
}

#[test]
fn directories_list_every_file() {
    let volume = volume();
    assert_eq!(volume.read_dir("many").unwrap().len(), 101);
    assert_eq!(volume.read_dir("many/d098").unwrap().len(), 1000);
}
