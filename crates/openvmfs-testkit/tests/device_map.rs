//! `Disk::map` says where on the devices the bytes of a virtual disk are. These tests read every mapped range
//! straight from the device and compare it with what the disk returns: base disks, snapshot chains with discards
//! (ESXi 7.0.3 and 8.0 U3), a datastore on two devices, and files behind double indirection.

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

use openvmfs::{Backing, BlockSource, Disk, ErrorKind, FileSource, MappedRange, OpenOptions, Volume};
use openvmfs_testkit::{own_image, own_partial_image};

const MIB: u64 = 1 << 20;

fn whole(name: &str) -> Arc<dyn BlockSource> {
    Arc::new(FileSource::open(own_image(name)).unwrap())
}

fn partial(name: &str) -> Arc<dyn BlockSource> {
    Arc::new(own_partial_image(name))
}

/// Checks the map of one disk against the devices. Returns (device ranges, zero ranges, devices used).
fn check(devices: &[Arc<dyn BlockSource>], disk: &Disk, name: &str) -> (usize, usize, Vec<usize>) {
    let capacity = disk.capacity();
    let map = disk.map(0..capacity, usize::MAX).unwrap();

    // Without gaps, in order, covering the disk, and merged as far as possible.
    let mut expected_offset = 0;
    for (index, range) in map.iter().enumerate() {
        assert_eq!(range.offset, expected_offset, "{name}: range {index}");
        assert!(range.len > 0, "{name}: range {index}");
        expected_offset += range.len;
        if let Some(next) = map.get(index + 1) {
            let mergeable = match (range.backing, next.backing) {
                (Backing::Zero, Backing::Zero) => true,
                (
                    Backing::Device { device: a, offset: at },
                    Backing::Device {
                        device: b,
                        offset: then,
                    },
                ) => a == b && at + range.len == then,
                _ => false,
            };
            assert!(!mergeable, "{name}: ranges {index} and the next could be one");
        }
    }
    assert_eq!(expected_offset, capacity, "{name}");

    // In small pieces the map is the same.
    let mut paged: Vec<MappedRange> = Vec::new();
    let mut from = 0;
    while from < capacity {
        let page = disk.map(from..capacity, 3).unwrap();
        assert!(!page.is_empty() && page.len() <= 3, "{name}");
        assert_eq!(page[0].offset, from, "{name}");
        from = page.last().map(|range| range.offset + range.len).unwrap();
        paged.extend(page);
    }
    assert_eq!(paged, map, "{name}");
    // A part of the disk maps like the same part of the whole.
    if capacity > 3 * MIB {
        let part = disk.map(MIB + 512..3 * MIB - 512, usize::MAX).unwrap();
        assert_eq!(part.first().unwrap().offset, MIB + 512, "{name}");
        assert_eq!(
            part.iter().map(|range| range.len).sum::<u64>(),
            2 * MIB - 1024,
            "{name}"
        );
    }

    // What the map says is on a device is what the disk returns; what it calls zero reads as zeros.
    let mut device_ranges = Vec::new();
    let mut zero_ranges = 0;
    let mut used = Vec::new();
    let mut ours = vec![0u8; MIB as usize];
    let mut theirs = vec![0u8; MIB as usize];
    for range in &map {
        match range.backing {
            Backing::Device { device, offset } => {
                device_ranges.push(range.offset..range.offset + range.len);
                if !used.contains(&device) {
                    used.push(device);
                }
                let mut done = 0;
                while done < range.len {
                    let take = (range.len - done).min(MIB) as usize;
                    disk.read_exact_at(range.offset + done, &mut ours[..take]).unwrap();
                    devices[device]
                        .read_exact_at(offset + done, &mut theirs[..take])
                        .unwrap();
                    assert!(
                        ours[..take] == theirs[..take],
                        "{name}: disk offset {:#x}",
                        range.offset + done
                    );
                    done += take as u64;
                }
            }
            Backing::Zero => {
                zero_ranges += 1;
                // The ends of every zero range; reading terabytes of holes in full would take too long.
                for at in [range.offset, range.offset + range.len - range.len.min(4096)] {
                    let take = range.len.min(4096) as usize;
                    disk.read_exact_at(at, &mut ours[..take]).unwrap();
                    assert!(
                        ours[..take].iter().all(|&byte| byte == 0),
                        "{name}: disk offset {at:#x}"
                    );
                }
            }
        }
    }
    // The ranges on devices are exactly the disk's data ranges.
    assert_eq!(device_ranges.len(), map.len() - zero_ranges);
    let mut merged: Vec<std::ops::Range<u64>> = Vec::new();
    for range in device_ranges {
        match merged.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => merged.push(range),
        }
    }
    assert_eq!(merged, disk.data_ranges(), "{name}");
    (map.len() - zero_ranges, zero_ranges, used)
}

#[test]
fn base_disks() {
    let device = whole("fixture-zero-v1");
    let volume = Volume::open(device.clone()).unwrap();
    for name in ["thin", "lazy", "eager"] {
        let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
        let (on_device, _, used) = check(std::slice::from_ref(&device), &disk, name);
        assert!(on_device > 0, "{name}");
        assert_eq!(used, [0], "{name}");
    }
}

#[test]
fn snapshot_chains_with_discards() {
    // (fixture, descriptors): three levels on ESXi 7.0.3 and on ESXi 8.0 U3.
    let cases = [
        (
            "fixture-chain-v2",
            vec![
                "guest/thin-g.vmdk",
                "guest/thin-g-000001.vmdk",
                "guest/thin-g-000002.vmdk",
                "guest/thin-g-000003.vmdk",
            ],
        ),
        (
            "fixture-chain-esxi8-v1",
            vec![
                "guest/t.vmdk",
                "guest/t-000001.vmdk",
                "guest/t-000002.vmdk",
                "guest/t-000003.vmdk",
            ],
        ),
    ];
    for (fixture, descriptors) in cases {
        let device = partial(fixture);
        let volume = Volume::open(device.clone()).unwrap();
        let mut zero_ranges_of_levels = Vec::new();
        for descriptor in descriptors {
            let disk = volume.open_disk(descriptor).unwrap();
            let (on_device, zero, _) = check(std::slice::from_ref(&device), &disk, descriptor);
            assert!(on_device > 0, "{descriptor}");
            zero_ranges_of_levels.push(zero);
        }
        // The guest's discards cut holes into what the base holds.
        assert!(
            zero_ranges_of_levels[1..]
                .iter()
                .any(|&zero| zero > zero_ranges_of_levels[0]),
            "{fixture}"
        );
    }
}

#[test]
fn a_datastore_on_two_devices_names_the_device() {
    let devices: Vec<Arc<dyn BlockSource>> = vec![whole("fixture-span-v1-dev1"), whole("fixture-span-v1-dev2")];
    let volume = Volume::open_devices(devices.clone(), &OpenOptions::default()).unwrap();
    let mut used_by_all = Vec::new();
    for name in ["lazy", "thin"] {
        let disk = volume.open_disk(&format!("d/{name}.vmdk")).unwrap();
        let (_, _, used) = check(&devices, &disk, name);
        used_by_all.extend(used);
    }
    assert!(used_by_all.contains(&0) && used_by_all.contains(&1), "{used_by_all:?}");
}

#[test]
fn huge_sparse_disks_map_in_a_few_ranges() {
    let device = partial("fixture-double-v1");
    let volume = Volume::open(device.clone()).unwrap();
    let disk = volume.open_disk("dbl/huge.vmdk").unwrap();
    let (on_device, zero, _) = check(&[device], &disk, "huge");
    // Five written places in 4 TiB, separated by holes.
    assert_eq!((on_device, zero), (5, 4));
}

#[test]
fn ranges_outside_the_disk_and_replayed_volumes_are_refused() {
    let device = whole("fixture-zero-v1");
    let volume = Volume::open(device.clone()).unwrap();
    let disk = volume.open_disk("pat/thin.vmdk").unwrap();
    let capacity = disk.capacity();
    assert_eq!(
        disk.map(0..capacity + 1, 10).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert!(disk.map(capacity..capacity, 10).unwrap().is_empty());
    assert!(disk.map(0..capacity, 0).unwrap().is_empty());

    let options = OpenOptions {
        replay_journal: true,
        ..OpenOptions::default()
    };
    let replayed = Volume::open_devices(vec![device], &options).unwrap();
    let disk = replayed.open_disk("pat/thin.vmdk").unwrap();
    assert_eq!(disk.map(0..capacity, 10).unwrap_err().kind(), ErrorKind::InvalidInput);
}
