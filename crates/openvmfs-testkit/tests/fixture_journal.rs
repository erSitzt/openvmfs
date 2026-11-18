//! Reads `fixtures/vmfs6/fixture-journal-v1`: a storage snapshot of a datastore whose VM had just written to a thin
//! disk, with no VM snapshot. Two block allocations of that disk were still waiting in the host's journal.
//!
//! The guest wrote chunk `i` (256 KiB into a never-written part of the disk) and then overwrote sector 0 with the
//! number `i`, each with fsync, up to 1700. See `docs/consistency.md`.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::single_range_in_vec_init
)]

use std::sync::Arc;

use openvmfs::{BlockSource, Disk, ErrorKind, OpenOptions, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, sha256_of};

const CHUNK: u64 = 256 << 10;
const MIB: u64 = 1 << 20;

fn open(options: &OpenOptions) -> Volume {
    Volume::open_with(Arc::new(own_partial_image("fixture-journal-v1")), options).expect("open fixture")
}

/// The number in sector 0, and the first chunk up to it that is not on the disk in full.
fn marker_and_first_missing(target: &Disk, source: &Disk) -> (u64, Option<u64>) {
    let mut sector = [0u8; 512];
    target.read_exact_at(0, &mut sector).unwrap();
    let text = std::str::from_utf8(sector.split(|&b| b == b'\n').next().unwrap()).unwrap();
    let marker: u64 = text.parse().unwrap();
    let (mut got, mut want) = (vec![0u8; CHUNK as usize], vec![0u8; CHUNK as usize]);
    for i in 1..=marker {
        target.read_exact_at((i + 4) * CHUNK, &mut got).unwrap();
        source.read_exact_at((i % 256) * CHUNK, &mut want).unwrap();
        if got != want {
            return (marker, Some(i));
        }
    }
    (marker, None)
}

#[test]
fn the_journal_holds_two_pending_transactions() {
    let volume = open(&OpenOptions::default());
    let status = volume.journal_status().unwrap();
    assert_eq!(
        (status.journals, status.pending_transactions, status.replayed),
        (1, 2, false)
    );
    // By default the disk is refused: a host was writing to it.
    assert_eq!(volume.open_disk("probe/m.vmdk").unwrap_err().kind(), ErrorKind::InUse);
}

#[test]
fn read_as_it_is_the_disk_lacks_writes_that_the_marker_says_were_made() {
    let volume = open(&OpenOptions {
        accept_in_use: true,
        ..OpenOptions::default()
    });
    let target = volume.open_disk("probe/m.vmdk").unwrap();
    let source = volume.open_disk("probe/src.vmdk").unwrap();
    // The marker was written after chunk 1700, yet the chunks from 1696 on read as zeros: their blocks are not in
    // the block map yet. No crash of the guest's disk could leave it like this.
    assert_eq!(marker_and_first_missing(&target, &source), (1700, Some(1696)));
    assert_eq!(target.data_ranges(), &[0..425 * MIB]);
}

#[test]
fn with_the_journal_replayed_the_disk_is_complete() {
    let volume = open(&OpenOptions {
        replay_journal: true,
        ..OpenOptions::default()
    });
    let status = volume.journal_status().unwrap();
    assert_eq!((status.pending_transactions, status.replayed), (2, true));

    let target = volume.open_disk("probe/m.vmdk").unwrap();
    let source = volume.open_disk("probe/src.vmdk").unwrap();
    assert_eq!(marker_and_first_missing(&target, &source), (1700, None));
    assert_eq!(target.data_ranges(), &[0..427 * MIB]);
    // The file's descriptor now counts the two blocks as well.
    assert_eq!(volume.stat("probe/m-flat.vmdk").unwrap().blocks, 427);

    // An image taken seven minutes later, with nothing pending, holds exactly this disk.
    let truth =
        std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-journal-v1/truth/sha256.txt")).unwrap();
    assert_eq!(sha256_of(&target), truth.split_whitespace().next().unwrap());
}
