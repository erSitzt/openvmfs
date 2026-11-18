//! Reads `fixtures/vmfs6/fixture-chain-v1`: disks with one VM snapshot each, so every disk is a base plus a SESparse
//! delta written by ESXi 7.0.3 while a guest was writing.
//!
//! The expected content of a chain is what ESXi itself produced when it consolidated the chain into a clone
//! (`vmkfstools -i`); the clones are on the same volume, and their hashes are in `truth/sha256.txt`.

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

use openvmfs::{BlockSource, ErrorKind, Volume};
use openvmfs_testkit::{own_image, repo_root, sha256_of};

const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open_path(own_image("fixture-chain-v1")).expect("open fixture")
}

fn esxi_sha256(path: &str) -> String {
    let truth = std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-chain-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

#[test]
fn a_chain_reads_like_the_clone_esxi_consolidated_from_it() {
    let volume = volume();
    // 156 MiB were written before the snapshot (base), 224 MiB after it (delta).
    let chain = volume.open_disk("guest/grow-000001.vmdk").unwrap();
    assert_eq!(chain.capacity(), 400 * MIB);
    assert_eq!(sha256_of(&chain), esxi_sha256("clone/grow-now-flat.vmdk"));
    assert_eq!(chain.data_ranges(), &[0..380 * MIB]);

    // The clone itself, a plain thin disk, reads the same.
    let clone = volume.open_disk("clone/grow-now.vmdk").unwrap();
    assert_eq!(sha256_of(&clone), esxi_sha256("clone/grow-now-flat.vmdk"));
}

#[test]
fn the_base_alone_is_the_disk_as_it_was_when_the_snapshot_was_taken() {
    let volume = volume();
    let base = volume.open_disk("guest/grow.vmdk").unwrap();
    assert_eq!(sha256_of(&base), esxi_sha256("guest/grow-flat.vmdk"));
    assert_eq!(base.data_ranges(), &[0..156 * MIB]);
    assert_eq!(base.chain().len(), 1);
}

#[test]
fn the_chain_lists_its_links_newest_first() {
    let volume = volume();
    let chain = volume.open_disk("guest/grow-000001.vmdk").unwrap();
    let links: Vec<(&str, &str, u32)> = chain
        .chain()
        .iter()
        .map(|link| {
            (
                link.descriptor_path.as_str(),
                link.descriptor.create_type.as_str(),
                link.descriptor.cid,
            )
        })
        .collect();
    assert_eq!(
        links,
        [
            ("guest/grow-000001.vmdk", "seSparse", 0xdd4a_9120),
            ("guest/grow.vmdk", "vmfs", 0x7b55_5d49)
        ]
    );
    assert_eq!(chain.descriptor().parent_cid, 0x7b55_5d49);
    assert_eq!(chain.descriptor_path(), "guest/grow-000001.vmdk");
}

#[test]
fn an_empty_delta_changes_nothing() {
    let volume = volume();
    for name in ["thin-g", "lazy-g", "src"] {
        let chain = volume.open_disk(&format!("guest/{name}-000001.vmdk")).unwrap();
        let base = volume.open_disk(&format!("guest/{name}.vmdk")).unwrap();
        assert_eq!(chain.data_ranges(), base.data_ranges(), "{name}");
        assert_eq!(sha256_of(&chain), sha256_of(&base), "{name}");
    }
    assert_eq!(
        sha256_of(&volume.open_disk("guest/thin-g-000001.vmdk").unwrap()),
        esxi_sha256("clone/thin-g-now-flat.vmdk")
    );
}

#[test]
fn reads_may_cross_between_delta_and_base() {
    let volume = volume();
    let chain = volume.open_disk("guest/grow-000001.vmdk").unwrap();
    let base = volume.open_disk("guest/grow.vmdk").unwrap();
    // The delta starts at 156 MiB. A read across that point takes its first half from the base.
    let mut across = vec![0u8; 8192];
    chain.read_exact_at(156 * MIB - 4096, &mut across).unwrap();
    let mut from_base = vec![0u8; 4096];
    base.read_exact_at(156 * MIB - 4096, &mut from_base).unwrap();
    assert_eq!(&across[..4096], &from_base[..]);
    // In the base alone, the second half is unallocated; in the chain it holds data.
    base.read_exact_at(156 * MIB, &mut from_base).unwrap();
    assert!(from_base.iter().all(|&b| b == 0));
    assert!(across[4096..].iter().any(|&b| b != 0));

    let mut beyond = [0u8; 1];
    assert_eq!(
        chain.read_exact_at(400 * MIB, &mut beyond).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn a_delta_file_is_not_a_descriptor() {
    let volume = volume();
    assert_eq!(
        volume.open_disk("guest/grow-000001-sesparse.vmdk").unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    // As a plain file it reads byte for byte like on ESXi.
    let delta = volume.open_file("guest/grow-000001-sesparse.vmdk").unwrap();
    assert_eq!(sha256_of(&delta), esxi_sha256("guest/grow-000001-sesparse.vmdk"));
}
