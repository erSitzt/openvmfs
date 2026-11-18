//! Reads `fixtures/vmfs6/fixture-zero-v1`: a 4 GiB disk with a VMFS 6.82 datastore created by ESXi 7.0.3, filled
//! with junk, emptied, and then given test disks with reference data (see `fixtures/vmfs6/README.md`).
//!
//! Three independent sources agree here: the write plan below (what was written where), the SHA-256 values ESXi
//! computed when it read the files through VMFS (`truth/sha256.txt`), and the block map ESXi printed
//! (`truth/*.t0.txt`).

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

use openvmfs::{BlockSource, Extent, ExtentKind, FileSource, Volume};
use openvmfs_testkit::{own_image, repo_root, sha256_of, ExpectedDisk, JUNK_MAGIC, SECTOR};

const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open_path(own_image("fixture-zero-v1")).expect("open fixture")
}

/// The SHA-256 ESXi computed for `path` (`sha256sum` on the ESXi shell, reading through VMFS).
fn esxi_sha256(path: &str) -> String {
    let truth = std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-zero-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

/// What `fixtures/recipes/pattern.py write` was asked to write, as (first sector, count).
fn plan(name: &str) -> ExpectedDisk {
    let (disk_id, size, written): (u32, u64, Vec<(u64, u64)>) = match name {
        "thin" => (
            1,
            1024 * MIB,
            vec![(0, 2048), (10248, 16), (614400, 4096), (2095104, 2048)],
        ),
        "lazy" => (2, 300 * MIB, vec![(2049, 3), (204800, 2048), (614392, 8)]),
        "eager" => (3, 64 * MIB, vec![(20480, 2048)]),
        "big" => (
            4,
            1100 * MIB,
            vec![(0, 16), (1046528, 4096), (2150400, 2048), (2252799, 1)],
        ),
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
fn volume_on_a_disk_with_a_partition_table() {
    let info = volume().info();
    assert_eq!(info.label, "fz");
    assert_eq!(info.version, "6.82");
    assert_eq!(info.uuid.to_string(), "6afdfbfe-33b90712-daa4-0050568b5583");
    assert_eq!(info.lvm_volume.uuid.to_string(), "6afdfbfe-13f86ecc-ad3b-0050568b5583");
    // ESXi put the partition at sector 2048.
    assert_eq!(info.partition_offset, 2048 * 512);
    assert_eq!(info.capacity, 3840 * MIB);
}

#[test]
fn every_disk_reads_exactly_what_was_written_and_what_esxi_read() {
    let volume = volume();
    for name in ["thin", "lazy", "eager", "big"] {
        let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
        // Sector by sector against the write plan: reference data where it was written, zeros everywhere else.
        let ours = plan(name).assert_matches(&disk, name);
        // And the same bytes ESXi returned.
        assert_eq!(
            ours,
            esxi_sha256(&format!("pat/{name}-flat.vmdk")),
            "{name}: differs from ESXi"
        );
    }
}

#[test]
fn written_ranges_are_reported_as_data() {
    let volume = volume();
    for name in ["thin", "lazy", "eager", "big"] {
        let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
        let ranges = disk.data_ranges();
        for (start, count) in plan(name).written {
            let (from, to) = (start * 512, (start + count) * 512);
            assert!(
                ranges.iter().any(|range| range.start <= from && to <= range.end),
                "{name}: written sectors {start}+{count} are not inside a data range"
            );
        }
    }
    // A thin disk reports only the blocks that were written: 1 + 1 + 2 + 1 MiB.
    let thin = volume.open_disk("pat/thin.vmdk").unwrap();
    assert_eq!(thin.data_ranges().iter().map(|r| r.end - r.start).sum::<u64>(), 5 * MIB);
    // An eager-zeroed disk is data throughout.
    let eager = volume.open_disk("pat/eager.vmdk").unwrap();
    assert_eq!(eager.data_ranges(), &[0..64 * MIB]);
}

#[test]
fn block_map_of_the_lazy_zeroed_disk_equals_what_esxi_printed() {
    let volume = volume();
    let disk = volume.open_disk("pat/lazy.vmdk").unwrap();
    // `vmkfstools -t0 pat/lazy.vmdk` (truth/lazy.t0.txt): file offset, length, Z = to be zeroed, offset on the disk.
    let esxi: [(u64, u64, bool, u64); 6] = [
        (0, 1048576, true, 2166358016),
        (1048576, 1048576, false, 2167406592),
        (2097152, 102760448, true, 2168455168),
        (104857600, 1048576, false, 2271215616),
        (105906176, 207618048, true, 2272264192),
        (313524224, 1048576, false, 2479882240),
    ];
    let ours: Vec<(u64, u64, bool, u64)> = disk
        .extents()
        .iter()
        .map(|extent: &Extent| match extent.kind {
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
}

#[test]
fn stale_data_under_never_written_blocks_is_on_disk_but_never_returned() {
    let volume = volume();
    let raw = FileSource::open(own_image("fixture-zero-v1")).unwrap();
    let mut junk_blocks = 0;
    for name in ["lazy", "big"] {
        let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
        let mut sector = [0u8; SECTOR];
        for extent in disk.extents() {
            let ExtentKind::ToBeZeroed { volume_offset } = extent.kind else {
                continue;
            };
            // Sample the first sector of every MiB of the extent.
            for block in 0..extent.len / MIB {
                let device_offset = volume.device_offset(volume_offset + block * MIB).unwrap();
                raw.read_exact_at(device_offset, &mut sector).unwrap();
                if sector.starts_with(JUNK_MAGIC) {
                    junk_blocks += 1;
                }
                disk.read_exact_at(extent.offset + block * MIB, &mut sector).unwrap();
                assert!(
                    sector.iter().all(|&b| b == 0),
                    "{name}: stale data leaked at {:#x}",
                    extent.offset + block * MIB
                );
            }
        }
    }
    // The datastore was filled with junk before the disks were created, so the test is not vacuous.
    assert!(
        junk_blocks > 1000,
        "only {junk_blocks} never-written blocks hold junk on disk"
    );
}

#[test]
fn small_files_match_esxi() {
    let volume = volume();
    for path in [
        "files/resident.txt",
        "files/sub.bin",
        "files/blocks.bin",
        "files/many/f0001",
        "files/many/f0300",
    ] {
        assert_eq!(sha256_of(&volume.open_file(path).unwrap()), esxi_sha256(path), "{path}");
    }
    for name in ["thin", "lazy", "eager", "big"] {
        let path = format!("pat/{name}.vmdk");
        assert_eq!(
            sha256_of(&volume.open_file(&path).unwrap()),
            esxi_sha256(&path),
            "{path}"
        );
    }
    assert_eq!(volume.read_dir("files/many").unwrap().len(), 300);
    assert_eq!(volume.read_link("files/link").unwrap(), "resident.txt");
    assert_eq!(
        volume.open_file("files/resident.txt").unwrap().read_all(MIB).unwrap(),
        b"resident data\n"
    );
}
