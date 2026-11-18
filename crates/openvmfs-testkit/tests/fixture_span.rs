//! Reads `fixtures/vmfs6/fixture-span-v1`: a datastore that ESXi 7.0.3 created on one device (3 GiB) and then
//! extended onto a second one (2 GiB), with two virtual disks whose blocks lie on both.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use openvmfs::{BlockSource, ErrorKind, ExtentKind, FileSource, OpenOptions, Volume};
use openvmfs_testkit::{own_image, repo_root, ExpectedDisk};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn devices() -> Vec<Arc<dyn BlockSource>> {
    ["fixture-span-v1-dev1", "fixture-span-v1-dev2"]
        .iter()
        .map(|name| Arc::new(FileSource::open(own_image(name)).unwrap()) as Arc<dyn BlockSource>)
        .collect()
}

fn esxi_sha256(path: &str) -> String {
    let truth = std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-span-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

/// What `fixtures/recipes/pattern.py write` was asked to write, as (first sector, count).
fn plan(name: &str) -> ExpectedDisk {
    let (disk_id, size, written): (u32, u64, Vec<(u64, u64)>) = match name {
        "lazy" => (
            60,
            2600 * MIB,
            vec![
                (0, 2048),
                (522240, 4096),
                (1048576, 2048),
                (1570816, 2048),
                (2095104, 4096),
                (2619392, 2048),
                (3143680, 2048),
                (3667968, 4096),
                (4192256, 2048),
                (4716544, 2048),
                (5322752, 2048),
            ],
        ),
        "thin" => (61, GIB, vec![(0, 16), (1048576, 2048), (2095104, 2048)]),
        other => panic!("no plan for {other}"),
    };
    ExpectedDisk {
        disk_id,
        generation: 1,
        size,
        written,
    }
}

#[test]
fn the_datastore_is_assembled_from_both_devices_in_any_order() {
    for reversed in [false, true] {
        let mut devices = devices();
        if reversed {
            devices.reverse();
        }
        let volume = Volume::open_devices(devices, &OpenOptions::default()).unwrap();
        let info = volume.info();
        assert_eq!(info.label, "sp");
        assert_eq!(info.capacity, 4608 * MIB);
        assert_eq!(volume.device_count(), 2);
        assert_eq!(info.lvm_volume.device_count, 2);
        assert_eq!(
            info.lvm_volume.device_names,
            ["mpx.vmhba0:C0:T8:L0:1", "mpx.vmhba0:C0:T9:L0:1"]
        );
        // The first device holds 2.75 GiB of the volume, the second the rest.
        let shares: Vec<(u64, u64)> = info
            .lvm_devices
            .iter()
            .map(|d| (d.volume_start, d.volume_len))
            .collect();
        assert_eq!(shares, [(0, 2816 * MIB), (2816 * MIB, 1792 * MIB)]);
        for name in ["lazy", "thin"] {
            let disk = volume.open_disk(&format!("d/{name}.vmdk")).unwrap();
            let hash = plan(name).assert_matches(&disk, name);
            assert_eq!(hash, esxi_sha256(&format!("d/{name}-flat.vmdk")), "{name}");
        }
    }
}

#[test]
fn a_disk_has_blocks_on_both_devices_where_esxi_says_they_are() {
    let volume = Volume::open_devices(devices(), &OpenOptions::default()).unwrap();
    let disk = volume.open_disk("d/lazy.vmdk").unwrap();
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

    // `vmkfstools -t0` prints offsets on the device that holds the block, without naming the device.
    let truth = std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-span-v1/truth/lazy.t0.txt")).unwrap();
    let mut esxi = Vec::new();
    for line in truth.lines() {
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
fn one_device_alone_is_refused_with_the_names_of_all_of_them() {
    for device in devices() {
        let error = Volume::open(device).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("spans 2 devices"), "{error}");
    }
    // The first device knows what the others are called.
    let first = devices().remove(0);
    let error = Volume::open(first).unwrap_err().to_string();
    assert!(error.contains("mpx.vmhba0:C0:T9:L0:1"), "{error}");
}
