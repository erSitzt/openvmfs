//! Reads `fixtures/vmfs6/fixture-merge-esxi8-v1`: snapshot chains of eleven deltas (ESXi 8.0 U3) in which snapshots
//! were deleted while the VM ran, so ESXi merged deltas into their parent deltas. Ranges the guest had discarded in
//! a merged delta are zero grains (type 2) afterwards.

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

use openvmfs::{BlockSource, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, sha256_hex};

const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-merge-esxi8-v1"))).expect("open fixture")
}

/// SHA-256 of single megabytes of ESXi's own consolidation of each level (`vmkfstools -i`), as (disk, MiB, hash).
fn truth() -> Vec<(String, u64, String)> {
    let path = repo_root().join("fixtures/vmfs6/fixture-merge-esxi8-v1/truth/mib.sha256.txt");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields[1].to_owned(), fields[2].parse().unwrap(), fields[0].to_owned())
        })
        .collect()
}

fn mib_of(volume: &Volume, descriptor: &str, mib: u64) -> Vec<u8> {
    let disk = volume.open_disk(descriptor).unwrap();
    let mut data = vec![0u8; MIB as usize];
    disk.read_exact_at(mib * MIB, &mut data).unwrap();
    data
}

#[test]
fn every_recorded_megabyte_reads_as_in_esxis_consolidation() {
    let volume = volume();
    let truth = truth();
    assert_eq!(truth.len(), 9);
    for (descriptor, mib, hash) in truth {
        assert_eq!(
            sha256_hex(&mib_of(&volume, &descriptor, mib)),
            hash,
            "{descriptor} at {mib} MiB"
        );
    }
}

#[test]
fn a_zero_grain_reads_as_zeros_although_the_level_below_has_data_there() {
    let volume = volume();
    // The guest discarded these megabytes; deleting the snapshot merged that delta into `pv-000012`.
    for mib in [98, 292] {
        assert!(
            mib_of(&volume, "misc/pv-000012.vmdk", mib).iter().all(|&b| b == 0),
            "{mib}"
        );
        assert!(
            mib_of(&volume, "misc/pv-000006.vmdk", mib).iter().all(|&b| b == 0),
            "{mib}"
        );
        // One level down the data is still there: from the base at 98 MiB, from an older delta at 292 MiB.
        assert!(
            mib_of(&volume, "misc/pv-000011.vmdk", mib).iter().any(|&b| b != 0),
            "{mib}"
        );
    }
    // The megabyte before was not discarded.
    assert!(mib_of(&volume, "misc/pv-000012.vmdk", 97).iter().any(|&b| b != 0));
}

#[test]
fn the_chain_has_eleven_deltas_over_its_base() {
    let disk = volume().open_disk("misc/pv-000006.vmdk").unwrap();
    assert_eq!(disk.capacity(), 2048 * MIB);
    assert_eq!(disk.chain().len(), 12);
}
