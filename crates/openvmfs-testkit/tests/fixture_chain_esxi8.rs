//! Reads `fixtures/vmfs6/fixture-chain-esxi8-v1`: a disk with three VM snapshots made by ESXi 8.0 U3, written and
//! discarded by a guest between them. ESXi 8 sets a header field of its SESparse deltas to 0x200 where ESXi 7 writes
//! 0; everything else is the same. The reference for each level is ESXi's own consolidation (`vmkfstools -i`).

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
use openvmfs_testkit::{own_partial_image, repo_root, sha256_of};

const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-chain-esxi8-v1"))).expect("open fixture")
}

fn esxi_sha256(path: &str) -> String {
    let truth =
        std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-chain-esxi8-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

#[test]
fn every_level_reads_like_esxi_8_consolidated_it() {
    let volume = volume();
    let base = volume.open_disk("guest/t.vmdk").unwrap();
    assert_eq!(sha256_of(&base), esxi_sha256("guest/t-flat.vmdk"));
    for level in 1..=3 {
        let chain = volume.open_disk(&format!("guest/t-00000{level}.vmdk")).unwrap();
        assert_eq!(chain.chain().len(), level + 1);
        assert_eq!(
            sha256_of(&chain),
            esxi_sha256(&format!("clone8/at{level}-flat.vmdk")),
            "level {level}"
        );
    }
}

#[test]
fn a_discard_on_esxi_8_reads_as_zeros_too() {
    let volume = volume();
    let base = volume.open_disk("guest/t.vmdk").unwrap();
    let level2 = volume.open_disk("guest/t-000002.vmdk").unwrap();
    let mut sector = [0u8; 512];
    // The base holds data at 10 MiB; the guest discarded that megabyte after the first snapshot.
    base.read_exact_at(10 * MIB, &mut sector).unwrap();
    assert!(sector.iter().any(|&byte| byte != 0));
    level2.read_exact_at(10 * MIB, &mut sector).unwrap();
    assert!(sector.iter().all(|&byte| byte == 0));
}
