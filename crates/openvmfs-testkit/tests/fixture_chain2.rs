//! Reads `fixtures/vmfs6/fixture-chain-v2`: a disk with three VM snapshots, written by a guest that also discarded
//! ranges (SCSI unmap) between them. See `fixtures/vmfs6/README.md` for what was done at each level.
//!
//! The expected content at each level is the clone ESXi consolidated from it (`vmkfstools -i`). The image is
//! partial; reads outside the captured ranges fail.

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

use openvmfs::{BlockSource, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, sha256_of, PATTERN_MAGIC};

const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-chain-v2"))).expect("open fixture")
}

fn esxi_sha256(fixture: &str, path: &str) -> String {
    let file = repo_root().join(format!("fixtures/vmfs6/{fixture}/truth/sha256.txt"));
    let truth = std::fs::read_to_string(file).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

#[test]
fn every_snapshot_level_reads_like_esxi_consolidated_it() {
    let volume = volume();
    let base = volume.open_disk("guest/thin-g.vmdk").unwrap();
    assert_eq!(
        sha256_of(&base),
        esxi_sha256("fixture-chain-v2", "guest/thin-g-flat.vmdk")
    );
    for level in 1..=3 {
        let chain = volume.open_disk(&format!("guest/thin-g-00000{level}.vmdk")).unwrap();
        assert_eq!(chain.chain().len(), level + 1);
        assert_eq!(
            sha256_of(&chain),
            esxi_sha256("fixture-chain-v2", &format!("clone/thin-g-at{level}-flat.vmdk")),
            "level {level} differs from ESXi's consolidated clone"
        );
    }
}

#[test]
fn a_discarded_range_reads_as_zeros_although_the_base_holds_data() {
    let volume = volume();
    let base = volume.open_disk("guest/thin-g.vmdk").unwrap();
    let level1 = volume.open_disk("guest/thin-g-000001.vmdk").unwrap();
    let mut sector = [0u8; 512];
    // The base has reference data at 10 MiB. In the first delta the guest discarded that MiB.
    base.read_exact_at(10 * MIB, &mut sector).unwrap();
    assert!(sector.starts_with(PATTERN_MAGIC));
    level1.read_exact_at(10 * MIB, &mut sector).unwrap();
    assert!(sector.iter().all(|&b| b == 0));
    // Data that was written and then discarded inside the same delta is gone too (21 MiB), its neighbour stays.
    level1.read_exact_at(21 * MIB, &mut sector).unwrap();
    assert!(sector.iter().all(|&b| b == 0));
    level1.read_exact_at(20 * MIB, &mut sector).unwrap();
    assert!(sector.starts_with(PATTERN_MAGIC));
    assert_eq!(level1.data_ranges(), &[0..MIB, 20 * MIB..21 * MIB, 48 * MIB..49 * MIB]);
}

#[test]
fn a_later_delta_wins_over_an_earlier_one_grain_by_grain() {
    let volume = volume();
    let level2 = volume.open_disk("guest/thin-g-000002.vmdk").unwrap();
    let mut sector = [0u8; 512];
    // In the second delta the guest wrote 8 sectors at sector 20544, inside the MiB discarded one level below.
    level2.read_exact_at(20544 * 512, &mut sector).unwrap();
    assert!(sector.starts_with(PATTERN_MAGIC));
    // The rest of that MiB still comes from the discard in the first delta, not from the base.
    level2.read_exact_at(10 * MIB, &mut sector).unwrap();
    assert!(sector.iter().all(|&b| b == 0));
    level2.read_exact_at(20552 * 512, &mut sector).unwrap();
    assert!(sector.iter().all(|&b| b == 0));
    // It also discarded 20 MiB, which the first delta had written.
    level2.read_exact_at(20 * MIB, &mut sector).unwrap();
    assert!(sector.iter().all(|&b| b == 0));
    assert_eq!(
        level2.data_ranges(),
        &[0..MIB, 20544 * 512..20552 * 512, 30 * MIB..31 * MIB, 48 * MIB..49 * MIB]
    );
}

#[test]
fn links_carry_the_content_id_of_their_parent() {
    let volume = volume();
    let chain = volume.open_disk("guest/thin-g-000003.vmdk").unwrap();
    let ids: Vec<(u32, u32)> = chain
        .chain()
        .iter()
        .map(|link| (link.descriptor.cid, link.descriptor.parent_cid))
        .collect();
    assert_eq!(
        ids,
        [
            (0x18ce_a210, 0x58c0_0d76),
            (0x58c0_0d76, 0xa063_edd0),
            (0xa063_edd0, 0x795a_2cf3),
            (0x795a_2cf3, 0xffff_ffff)
        ]
    );
}

#[test]
fn empty_deltas_on_top_of_a_chain_change_nothing() {
    let volume = volume();
    // `grow` got two more snapshots without further writes: base, one delta with 224 MiB, two empty deltas.
    let chain = volume.open_disk("guest/grow-000003.vmdk").unwrap();
    assert_eq!(chain.chain().len(), 4);
    assert_eq!(
        sha256_of(&chain),
        esxi_sha256("fixture-chain-v1", "clone/grow-now-flat.vmdk")
    );
    assert_eq!(chain.data_ranges(), &[0..380 * MIB]);
}
