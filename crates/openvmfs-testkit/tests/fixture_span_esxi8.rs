//! Reads `fixtures/vmfs6/fixture-span-esxi8-v1`: a datastore that ESXi 8.0 U3 created on one device (4 GiB) and
//! then extended onto a second one of the same size, with a lazy-zeroed disk whose blocks lie on both.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use openvmfs::{BlockSource, ErrorKind, ExtentKind, FileSource, OpenOptions, Volume};
use openvmfs_testkit::{own_image, repo_root, sha256_of};

const MIB: u64 = 1 << 20;

fn devices() -> Vec<Arc<dyn BlockSource>> {
    ["fixture-span-esxi8-v1-dev1", "fixture-span-esxi8-v1-dev2"]
        .iter()
        .map(|name| Arc::new(FileSource::open(own_image(name)).unwrap()) as Arc<dyn BlockSource>)
        .collect()
}

fn truth(name: &str) -> String {
    std::fs::read_to_string(
        repo_root()
            .join("fixtures/vmfs6/fixture-span-esxi8-v1/truth")
            .join(name),
    )
    .unwrap()
}

fn esxi_sha256(path: &str) -> String {
    truth("sha256.txt")
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

#[test]
fn both_disks_have_esxis_hashes_whatever_order_the_devices_are_given_in() {
    for reversed in [false, true] {
        let mut devices = devices();
        if reversed {
            devices.reverse();
        }
        let volume = Volume::open_devices(devices, &OpenOptions::default()).unwrap();
        let info = volume.info();
        assert_eq!(info.label, "sp8");
        assert_eq!(info.capacity, 7680 * MIB);
        assert_eq!(volume.device_count(), 2);
        let shares: Vec<(u64, u64)> = info
            .lvm_devices
            .iter()
            .map(|d| (d.volume_start, d.volume_len))
            .collect();
        assert_eq!(shares, [(0, 3840 * MIB), (3840 * MIB, 3840 * MIB)]);
        for (name, size) in [("lazy", 4500 * MIB), ("thin", 1024 * MIB)] {
            let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
            assert_eq!(disk.capacity(), size);
            assert_eq!(
                sha256_of(&disk),
                esxi_sha256(&format!("pat/{name}-flat.vmdk")),
                "{name}"
            );
        }
    }
}

#[test]
fn a_disk_has_blocks_on_both_devices_where_esxi_says_they_are() {
    let volume = Volume::open_devices(devices(), &OpenOptions::default()).unwrap();
    let disk = volume.open_disk("pat/lazy.vmdk").unwrap();
    let mut used = BTreeSet::new();
    let mut ours = Vec::new();
    for extent in disk.extents() {
        if let ExtentKind::Data { volume_offset } = extent.kind {
            let (device, offset) = volume.device_location(volume_offset).unwrap();
            used.insert(device);
            ours.push((extent.offset, extent.len, offset));
        }
    }
    assert_eq!(used.into_iter().collect::<Vec<_>>(), [0, 1]);

    // `vmkfstools -t0` prints offsets on the device that holds the block.
    let mut esxi = Vec::new();
    for line in truth("lazy.t0.txt").lines() {
        let Some((file, device)) = line.split_once("--> [VMFS -- ") else {
            continue;
        };
        let numbers = |text: &str| -> Vec<u64> {
            text.split(|c: char| !c.is_ascii_digit())
                .filter(|part| !part.is_empty())
                .map(|n| n.parse().unwrap())
                .collect()
        };
        let (file, device) = (numbers(file), numbers(device.rsplit_once(":(").unwrap().1));
        esxi.push((file[0], file[1], device[0]));
    }
    assert!(esxi.len() > 5);
    assert_eq!(ours, esxi);
}

#[test]
fn one_device_alone_is_refused() {
    for device in devices() {
        let error = Volume::open(device).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("spans 2 devices"), "{error}");
    }
}
