//! Reads `fixtures/vmfs6/fixture-pb2-esxi8-v1`: the datastore of `fixture-many-esxi8-v1` after ESXi 8.0 U3 was made
//! to grow its sub-block pool to 131,840 sub-blocks (`fixtures/recipes/make_pb2.py`). `.sbc.sf` is then longer than
//! 8 GiB and needs a second pointer block in `.pb2.sf`, whose address the older tools read in two different ways.
//! Without that pointer block nothing on the volume can be opened.

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
    Volume::open(Arc::new(own_partial_image("fixture-pb2-esxi8-v1"))).expect("open fixture")
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
    // Written when the pool was at 67,328 sub-blocks, and files of the fixture this datastore came from.
    check(&volume, 0, 0);
    let old = volume.open_file("many/d098/f0256").unwrap();
    assert_eq!(old.read_all(1 << 20).unwrap().len(), 8192);
    // Written while the pool grew from 130,816 to 131,840 sub-blocks; 131,072 of them fit below 8 GiB.
    for (round, index) in [(249, 0), (250, 0), (250, 506), (252, 0), (252, 506), (253, 1)] {
        check(&volume, round, index);
    }
}

#[test]
fn a_file_whose_pointer_block_lies_behind_the_8_gib_mark_reads_like_on_esxi() {
    let volume = volume();
    let file = volume.open_file("pb2/r252/big").unwrap();
    assert_eq!(file.stat().addressing, AddressingMode::Indirect);
    assert_eq!(file.stat().len, 400 << 20);
    let content = file.read_all(512 << 20).unwrap();
    // `sha256sum` on ESXi, truth/esxi.txt.
    assert_eq!(
        sha256_hex(&content),
        "a638749185c3337d2b0ba70ae4efe392e72e6a57104b5528a755115327540048"
    );
}

#[test]
fn directories_list_every_round() {
    let volume = volume();
    assert_eq!(volume.read_dir("pb2").unwrap().len(), 254);
    assert_eq!(volume.read_dir("pb2/r252").unwrap().len(), 508);
}
