//! Reads `fixtures/vmfs6/fixture-grow-esxi8-v1` and `fixture-grow-esxi7-v1`: a datastore that ESXi (8.0 U3 and
//! 7.0.3) created on a 4 GiB device and grew in place after the device had been enlarged to 8 GiB, the way a
//! datastore follows a LUN that was made larger.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects
)]

use std::sync::Arc;

use openvmfs::{ExtentKind, FileSource, Volume};
use openvmfs_testkit::{own_image, repo_root, sha256_of};

const MIB: u64 = 1 << 20;

const FIXTURES: [&str; 2] = ["fixture-grow-esxi8-v1", "fixture-grow-esxi7-v1"];

fn volume(fixture: &str) -> Volume {
    Volume::open(Arc::new(FileSource::open(own_image(fixture)).unwrap())).unwrap()
}

fn esxi_sha256(fixture: &str, path: &str) -> String {
    let truth = repo_root()
        .join("fixtures/vmfs6")
        .join(fixture)
        .join("truth/sha256.txt");
    std::fs::read_to_string(truth)
        .unwrap()
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

#[test]
fn the_volume_has_the_size_after_the_growth_and_every_disk_esxis_hash() {
    for fixture in FIXTURES {
        let volume = volume(fixture);
        assert_eq!(volume.info().capacity, 7936 * MIB);
        assert_eq!(volume.device_count(), 1);
        for name in ["l1", "l2", "t"] {
            let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
            assert_eq!(
                sha256_of(&disk),
                esxi_sha256(fixture, &format!("pat/{name}-flat.vmdk")),
                "{fixture} {name}"
            );
        }
    }
}

#[test]
fn the_disk_created_after_the_growth_lies_in_the_new_space() {
    for fixture in FIXTURES {
        let volume = volume(fixture);
        let disk = volume.open_disk("pat/l2.vmdk").unwrap();
        let behind_the_old_end = disk.extents().iter().any(|extent| match extent.kind {
            ExtentKind::Data { volume_offset } | ExtentKind::ToBeZeroed { volume_offset } => {
                volume.device_location(volume_offset).unwrap().1 > 4096 * MIB
            }
            ExtentKind::Hole => false,
        });
        assert!(behind_the_old_end, "{fixture}");
    }
}
