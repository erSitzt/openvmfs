//! Reads `fixtures/vmfs6/fixture-lfb-v1`: a 256 GiB datastore created by ESXi 7.0.3 that holds thick disks made of
//! large file blocks (512 MiB), written over recognisable junk (see `fixtures/vmfs6/README.md`).
//!
//! Only the parts of the device that matter were captured. The source used here fails on any read outside them, so
//! a passing test proves the reader looked only where real data was captured.

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

use openvmfs::{BlockSource, ErrorKind, ExtentKind, Volume};
use openvmfs_testkit::{own_partial_image, repo_root, sha256_of, ExpectedDisk, PartialSource, JUNK_MAGIC, SECTOR};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn open() -> (Arc<PartialSource>, Volume) {
    let device = Arc::new(own_partial_image("fixture-lfb-v1"));
    let volume = Volume::open(device.clone()).expect("open fixture");
    (device, volume)
}

fn esxi_sha256(path: &str) -> String {
    let truth = std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-lfb-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

/// What `fixtures/recipes/pattern.py write` was asked to write, as (first sector, count).
fn plan(name: &str) -> ExpectedDisk {
    let (disk_id, size, written): (u32, u64, Vec<(u64, u64)>) = match name {
        "lfb-lazy" => (
            21,
            2 * GIB,
            vec![(0, 8), (1048560, 32), (1433600, 2048), (3147777, 3), (4194303, 1)],
        ),
        "lfb-eager" => (22, GIB, vec![(2048, 2048), (1052672, 16)]),
        "lfb-mixed" => (23, 1300 * MIB, vec![(0, 16), (2097152, 2048), (2662399, 1)]),
        "thin2" => (24, 2 * GIB, vec![(4096, 8), (2097152, 2048), (4194296, 8)]),
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
fn a_large_datastore_opens() {
    let (device, volume) = open();
    let info = volume.info();
    assert_eq!(info.label, "f2");
    assert_eq!(info.version, "6.82");
    assert_eq!(info.uuid.to_string(), "6afe0153-138d6a42-11e3-0050568b5583");
    assert_eq!(device.len(), 256 * GIB);
    assert_eq!(info.partition_offset, MIB);
    assert!(info.capacity > 255 * GIB);
    assert_eq!(
        volume.open_file("files/note.txt").unwrap().read_all(MIB).unwrap(),
        b"large file block fixture\n"
    );
}

#[test]
fn disks_made_of_large_file_blocks_read_what_was_written_and_what_esxi_read() {
    let (_, volume) = open();
    for name in ["lfb-lazy", "lfb-eager", "lfb-mixed", "thin2"] {
        let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
        let ours = plan(name).assert_matches(&disk, name);
        assert_eq!(
            ours,
            esxi_sha256(&format!("pat/{name}-flat.vmdk")),
            "{name}: differs from ESXi"
        );
        let descriptor = format!("pat/{name}.vmdk");
        assert_eq!(
            sha256_of(&volume.open_file(&descriptor).unwrap()),
            esxi_sha256(&descriptor)
        );
    }
}

#[test]
fn block_map_of_a_disk_mixing_large_and_small_blocks_equals_what_esxi_printed() {
    let (_, volume) = open();
    let disk = volume.open_disk("pat/lfb-mixed.vmdk").unwrap();
    // `vmkfstools -t0 pat/lfb-mixed.vmdk` (truth/lfb-mixed.t0.txt), with ESXi's two adjacent "Z" lines merged:
    // file offset, length, Z = to be zeroed, offset on the disk. The first 1024 MiB are two large file blocks,
    // the remaining 276 MiB small file blocks far away on the device.
    let esxi: [(u64, u64, bool, u64); 5] = [
        (0, 1048576, false, 71959576576),
        (1048576, 536870912 + 535822336, true, 71960625152),
        (1073741824, 1048576, false, 206916550656),
        (1074790400, 287309824, true, 206917599232),
        (1362100224, 1048576, false, 207204909056),
    ];
    let ours: Vec<(u64, u64, bool, u64)> = disk
        .extents()
        .iter()
        .map(|extent| match extent.kind {
            ExtentKind::Data { volume_offset } => (
                extent.offset,
                extent.len,
                false,
                volume.device_offset(volume_offset).unwrap(),
            ),
            ExtentKind::ToBeZeroed { volume_offset } => (
                extent.offset,
                extent.len,
                true,
                volume.device_offset(volume_offset).unwrap(),
            ),
            ExtentKind::Hole => panic!("a thick disk has no holes"),
        })
        .collect();
    assert_eq!(ours, esxi);
    // Only the three written MiB are reported as data.
    assert_eq!(disk.data_ranges().iter().map(|r| r.end - r.start).sum::<u64>(), 3 * MIB);
}

#[test]
fn stale_data_under_never_written_parts_of_large_blocks_is_not_returned() {
    let (device, volume) = open();
    let disk = volume.open_disk("pat/lfb-mixed.vmdk").unwrap();
    let mut sector = [0u8; SECTOR];
    let mut junk_seen = 0;
    for extent in disk.extents() {
        let ExtentKind::ToBeZeroed { volume_offset } = extent.kind else {
            continue;
        };
        // The first MiB of each such extent was captured from the device.
        let device_offset = volume.device_offset(volume_offset).unwrap();
        assert!(device.covers(device_offset, MIB));
        device.read_exact_at(device_offset, &mut sector).unwrap();
        if sector.starts_with(JUNK_MAGIC) {
            junk_seen += 1;
        }
        disk.read_exact_at(extent.offset, &mut sector).unwrap();
        assert!(
            sector.iter().all(|&b| b == 0),
            "stale data leaked at {:#x}",
            extent.offset
        );
    }
    // The large file blocks of this disk were filled with junk before the disk was created.
    assert!(
        junk_seen >= 1,
        "no never-written extent of this disk holds junk on the device"
    );
}

#[test]
fn reads_outside_the_captured_parts_fail_instead_of_returning_zeros() {
    let (device, _) = open();
    let mut buf = [0u8; 512];
    assert_eq!(
        device.read_exact_at(100 * GIB, &mut buf).unwrap_err().kind(),
        ErrorKind::Io
    );
    assert!(device.read_exact_at(0, &mut buf).is_ok());
}
