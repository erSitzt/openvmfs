//! Reads `fixtures/vmfs6/fixture-guest-v1`: the datastore of `fixture-zero-v1`, imaged **while a VM was running**
//! that had written to a thin and a lazy-zeroed disk through its virtual SCSI controller.
//!
//! The image is partial (only what these tests read was kept); reads outside it fail. The expected hashes are the
//! ones ESXi computed after the VM had been powered off.

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

use openvmfs::{BlockSource, ErrorKind, ExtentKind, LockMode, OpenOptions, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, ExpectedDisk, PartialSource, JUNK_MAGIC, SECTOR};

const MIB: u64 = 1 << 20;

/// The image was taken while the VM was running, without a VM snapshot: its disks were open for writing. The guest
/// had finished and synced its writes long before, which is why they can be compared with ESXi here at all.
fn open() -> (Arc<PartialSource>, Volume) {
    let device = Arc::new(own_partial_image("fixture-guest-v1"));
    let options = OpenOptions {
        accept_in_use: true,
        ..OpenOptions::default()
    };
    let volume = Volume::open_with(device.clone(), &options).expect("open fixture");
    (device, volume)
}

fn esxi_sha256(path: &str) -> String {
    let truth = std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-guest-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

/// The guest copied these sector ranges from the source disk, whose every sector holds pattern 30 for its own
/// sector number, to the same sector numbers of the target disks.
fn plan(name: &str) -> ExpectedDisk {
    let (size, written): (u64, Vec<(u64, u64)>) = match name {
        "src" => (64 * MIB, vec![(0, 131072)]),
        "thin-g" => (256 * MIB, vec![(0, 16), (20480, 2048), (100000, 5)]),
        "lazy-g" => (128 * MIB, vec![(2049, 3), (40960, 2048)]),
        other => panic!("no plan for {other}"),
    };
    ExpectedDisk {
        disk_id: 30,
        generation: 1,
        size,
        written,
    }
}

#[test]
fn disks_of_a_running_vm_are_refused_unless_asked_for() {
    let device = Arc::new(own_partial_image("fixture-guest-v1"));
    let volume = Volume::open(device).unwrap();
    // The host that runs the VM holds its disks exclusively; that is recorded on disk.
    let flat = volume.stat("guest/thin-g-flat.vmdk").unwrap();
    assert_eq!(flat.lock, LockMode::Exclusive);
    assert_ne!(flat.lock_owner.to_string(), "00000000-00000000-0000-000000000000");
    let error = volume.open_disk("guest/thin-g.vmdk").unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InUse);
    assert!(error.to_string().contains("VM snapshot"), "{error}");
    // Reading the file as a file stays possible: the caller sees the lock in its metadata.
    assert!(volume.open_file("guest/thin-g-flat.vmdk").is_ok());
}

#[test]
fn the_host_running_the_vm_holds_a_heartbeat() {
    let (_, volume) = open();
    assert_eq!(volume.info().active_heartbeats, 1);
}

#[test]
fn disks_written_by_a_guest_read_what_the_guest_wrote_and_what_esxi_read() {
    let (_, volume) = open();
    for name in ["src", "thin-g", "lazy-g"] {
        let disk = volume.open_disk(&format!("guest/{name}.vmdk")).unwrap();
        let ours = plan(name).assert_matches(&disk, name);
        assert_eq!(
            ours,
            esxi_sha256(&format!("guest/{name}-flat.vmdk")),
            "{name}: differs from ESXi"
        );
    }
}

#[test]
fn a_partial_guest_write_clears_the_never_written_mark_of_its_block_only() {
    let (device, volume) = open();
    let disk = volume.open_disk("guest/lazy-g.vmdk").unwrap();
    // 128 blocks; the guest wrote three sectors into block 1 and all of block 20.
    let kinds: Vec<(u64, u64, bool)> = disk
        .extents()
        .iter()
        .map(|extent| {
            (
                extent.offset / MIB,
                extent.len / MIB,
                matches!(extent.kind, ExtentKind::Data { .. }),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            (0, 1, false),
            (1, 1, true),
            (2, 18, false),
            (20, 1, true),
            (21, 107, false)
        ]
    );

    // Under the never-written blocks the device still holds the junk the datastore was filled with.
    let mut sector = [0u8; SECTOR];
    let mut junk = 0;
    for extent in disk.extents() {
        let ExtentKind::ToBeZeroed { volume_offset } = extent.kind else {
            continue;
        };
        device
            .read_exact_at(volume.device_offset(volume_offset).unwrap(), &mut sector)
            .unwrap();
        junk += usize::from(sector.starts_with(JUNK_MAGIC));
    }
    assert_eq!(junk, 3);
}
