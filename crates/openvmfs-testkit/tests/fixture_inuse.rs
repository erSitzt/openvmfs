//! Reads `fixtures/vmfs6/fixture-inuse-v1`: a storage snapshot of a datastore whose VM was writing.
//!
//! The guest alternated two kinds of writes, each with fsync: chunk `i` (256 KiB into a part of a thin disk that
//! was never written) and a marker (sector 0, overwritten in place with the number `i`). In the middle of that a
//! VM snapshot was taken, and about a second later the storage snapshot that this image comes from.
//!
//! Without the VM snapshot, images of the same workload lacked the newest 8 to 11 MiB of chunks although the
//! marker said they had been written (`truth/marker-runs.jsonl`, `docs/consistency.md`). With it, the disk below
//! the snapshot is complete, and the delta the VM writes to is recognisable by its lock.

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

use openvmfs::{BlockSource, ErrorKind, LockMode, OpenOptions, Volume};
use openvmfs_testkit::own_partial_image;

const CHUNK: u64 = 256 << 10;
const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-inuse-v1"))).expect("open fixture")
}

#[test]
fn the_disk_below_the_vm_snapshot_is_complete_and_in_write_order() {
    let volume = volume();
    // Opens with default options: the host holds it read-only.
    let target = volume.open_disk("probe/m.vmdk").unwrap();
    let source = volume.open_disk("probe/src.vmdk").unwrap();
    assert_eq!(volume.stat("probe/m-flat.vmdk").unwrap().lock, LockMode::ReadOnly);

    let mut sector = [0u8; 512];
    target.read_exact_at(0, &mut sector).unwrap();
    let text = std::str::from_utf8(sector.split(|&b| b == b'\n').next().unwrap()).unwrap();
    let marker: u64 = text.parse().unwrap();
    assert_eq!(marker, 1401);

    // Every chunk up to the marker is there in full ...
    let (mut got, mut want) = (vec![0u8; CHUNK as usize], vec![0u8; CHUNK as usize]);
    for i in 1..=marker {
        target.read_exact_at((i + 4) * CHUNK, &mut got).unwrap();
        source.read_exact_at((i % 256) * CHUNK, &mut want).unwrap();
        assert!(
            got == want,
            "chunk {i} is missing or damaged although the marker says {marker}"
        );
    }
    // ... and nothing after the one that may have been under way.
    let end_of_data = target.data_ranges().last().unwrap().end;
    assert!(end_of_data <= ((marker + 1 + 4 + 1) * CHUNK).next_multiple_of(MIB));
}

#[test]
fn the_delta_the_vm_writes_to_is_refused_as_in_use() {
    let volume = volume();
    let delta = volume.stat("probe/m-000001-sesparse.vmdk").unwrap();
    assert_eq!(delta.lock, LockMode::Exclusive);
    let error = volume.open_disk("probe/m-000001.vmdk").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InUse);
    assert!(error.to_string().contains("m-000001-sesparse.vmdk"), "{error}");
}

#[test]
fn asking_for_it_gets_past_the_lock_but_not_past_an_unclean_delta() {
    let device = Arc::new(own_partial_image("fixture-inuse-v1"));
    let options = OpenOptions {
        accept_in_use: true,
        ..OpenOptions::default()
    };
    let volume = Volume::open_with(device, &options).unwrap();
    // The delta also carries the "replay journal" flag, which no option overrides.
    let error = volume.open_disk("probe/m-000001.vmdk").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported);
    assert!(error.to_string().contains("not closed cleanly"), "{error}");
}
