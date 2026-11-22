//! Reads `fixtures/vmfs6/fixture-span3-esxi8-v1`: a datastore that ESXi 8.0 U3 created on one device (4 GiB),
//! extended onto a second and a third, and then grew in place on the first device after that had been enlarged to
//! 8 GiB. The first device's new space lies at the end of the volume, behind the other two devices.

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
    [
        "fixture-span3-esxi8-v1-dev1",
        "fixture-span3-esxi8-v1-dev2",
        "fixture-span3-esxi8-v1-dev3",
    ]
    .iter()
    .map(|name| Arc::new(FileSource::open(own_image(name)).unwrap()) as Arc<dyn BlockSource>)
    .collect()
}

fn truth(name: &str) -> String {
    std::fs::read_to_string(
        repo_root()
            .join("fixtures/vmfs6/fixture-span3-esxi8-v1/truth")
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
fn all_disks_have_esxis_hashes_whatever_order_the_devices_are_given_in() {
    for rotation in 0..3 {
        let mut devices = devices();
        devices.rotate_left(rotation);
        let volume = Volume::open_devices(devices, &OpenOptions::default()).unwrap();
        let info = volume.info();
        assert_eq!(info.label, "s3");
        assert_eq!(info.capacity, 15616 * MIB);
        assert_eq!(volume.device_count(), 3);
        // The first device holds 7.75 GiB in two parts: where the volume starts, and at its end.
        let shares: Vec<(u64, u64)> = info
            .lvm_devices
            .iter()
            .map(|d| (d.volume_start, d.volume_len))
            .collect();
        assert_eq!(
            shares,
            [(0, 7936 * MIB), (3840 * MIB, 3840 * MIB), (7680 * MIB, 3840 * MIB)]
        );
        // Every disk once; in the other orders only the smallest one.
        let names: &[&str] = if rotation == 0 {
            &["l1", "l2", "l3", "l4", "t"]
        } else {
            &["t"]
        };
        for name in names {
            let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
            assert_eq!(
                sha256_of(&disk),
                esxi_sha256(&format!("pat/{name}-flat.vmdk")),
                "{name}"
            );
        }
    }
}

/// (offset in the file, length, offset on its device) of every data extent.
type Map = Vec<(u64, u64, u64)>;

/// The data extents of a disk as OpenVmfs sees them and as
/// `vmkfstools -t0` printed them, and the devices they lie on.
fn maps(volume: &Volume, name: &str) -> (Map, Map, BTreeSet<usize>) {
    let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
    let mut used = BTreeSet::new();
    let mut ours = Vec::new();
    for extent in disk.extents() {
        if let ExtentKind::Data { volume_offset } = extent.kind {
            let (device, offset) = volume.device_location(volume_offset).unwrap();
            used.insert(device);
            ours.push((extent.offset, extent.len, offset));
        }
    }
    let mut esxi = Vec::new();
    for line in truth(&format!("{name}.t0.txt")).lines() {
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
    (ours, esxi, used)
}

#[test]
fn blocks_are_where_esxi_says_also_in_the_part_the_first_device_grew_by() {
    let volume = Volume::open_devices(devices(), &OpenOptions::default()).unwrap();
    for (name, devices) in [("l2", vec![0, 1]), ("l3", vec![1, 2]), ("l4", vec![0, 2])] {
        let (ours, esxi, used) = maps(&volume, name);
        assert!(esxi.len() >= 4, "{name}");
        assert_eq!(ours, esxi, "{name}");
        assert_eq!(used.into_iter().collect::<Vec<_>>(), devices, "{name}");
    }
    // `l4` was created after the growth: it has data behind the first device's old end of 4 GiB.
    let (ours, _, _) = maps(&volume, "l4");
    assert!(ours.iter().any(|&(_, _, device_offset)| device_offset > 4096 * MIB));
}

#[test]
fn a_device_alone_is_refused() {
    for device in devices() {
        let error = Volume::open(device).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("spans 3 devices"), "{error}");
    }
}
