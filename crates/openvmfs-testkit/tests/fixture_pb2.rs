//! Reads `fixtures/vmfs6/fixture-pb2-v1`: a datastore on which ESXi 7.0.3 grew its sub-block pool past 131,072
//! sub-blocks (about 132,000 small files), so that `.sbc.sf` has a second pointer block in `.pb2.sf`.

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

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-pb2-v1"))).expect("open fixture")
}

/// File `index` of round `round` holds the hash of its own name 256 times (`make_pb2.py`).
fn check(volume: &Volume, round: u64, index: u64) {
    let path = format!("pb2/r{round:03}/f{index:03}");
    let file = volume.open_file(&path).unwrap();
    assert_eq!(file.stat().addressing, AddressingMode::SubBlocks, "{path}");
    let content = file.read_all(1 << 20).unwrap();
    assert_eq!(content.len(), 8192, "{path}");
    let want = sha256_hex(format!("openvmfs-pb2-{round}-{index}").as_bytes());
    for chunk in content.chunks(32) {
        let hex: String = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, want, "{path}");
    }
}

#[test]
fn the_sub_block_file_is_longer_than_one_pointer_block_covers() {
    let file = volume().open_file(".sbc.sf").unwrap();
    let stat = file.stat();
    // One pointer block covers 8192 blocks of 1 MiB.
    assert_eq!(stat.len, 8_644_591_616);
    assert!(stat.len > 8192 << 20);
}

#[test]
fn small_files_on_both_sides_of_the_8_gib_mark_read_as_written() {
    let volume = volume();
    // 131,072 sub-blocks fit below 8 GiB; round 447 was written into the 512 sub-blocks added last.
    for (round, index) in [
        (0, 0),
        (30, 100),
        (445, 0),
        (445, 400),
        (447, 0),
        (447, 250),
        (447, 506),
        (448, 1),
    ] {
        check(&volume, round, index);
    }
}

#[test]
fn a_file_whose_pointer_block_lies_behind_the_8_gib_mark_reads_like_on_esxi() {
    let volume = volume();
    let file = volume.open_file("pb2/r447/big").unwrap();
    assert_eq!(file.stat().addressing, AddressingMode::Indirect);
    assert_eq!(file.stat().len, 400 << 20);
    let content = file.read_all(512 << 20).unwrap();
    // `sha256sum` on ESXi, truth/esxi.txt.
    assert_eq!(
        sha256_hex(&content),
        "2a42bf35994179810caff96f4bb6fb207af3104669274c85c19ba3d3ed4abe6d"
    );
}

#[test]
fn directories_list_every_round() {
    let volume = volume();
    assert_eq!(volume.read_dir("pb2").unwrap().len(), 449);
    assert_eq!(volume.read_dir("pb2/r000").unwrap().len(), 600);
    assert_eq!(volume.read_dir("pb2/r447").unwrap().len(), 508);
}
