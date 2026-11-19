//! Reads `fixtures/vmfs6/fixture-double-v1`: two thin disks larger than 2.5 TiB on a 256 GiB datastore (ESXi 7.0.3).
//! Above that size a file's addresses no longer fit behind one level of pointer blocks, and ESXi switches the file to
//! double indirection (`zla 4304` in `vmkfstools -D`). The disks are far too large to hash; the reference is the
//! pattern that was written, sector by sector, and ESXi's block map.

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

use openvmfs::fs::AddressingMode;
use openvmfs::{BlockSource, Volume};
use openvmfs_testkit::{own_partial_image, pattern_sector, repo_root};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

/// A test disk: name, disk id, size, and what was written as (first sector, count).
type Plan = (&'static str, u32, u64, Vec<(u64, u64)>);

fn plans() -> [Plan; 2] {
    [
        // Writes at the start, at exactly 2.5 TiB (the first block behind double indirection), beyond it, and at
        // the very end.
        (
            "huge",
            31,
            4096 * GIB,
            vec![
                (0, 2048),
                (5368709120, 2048),
                (5637144576, 4096),
                (8371830784, 2048),
                (8589932544, 2048),
            ],
        ),
        // Two megabytes on either side of the 2.5 TiB line, and eight sectors in the last megabyte.
        (
            "edge",
            32,
            2600 * GIB,
            vec![(2048, 8), (5368707072, 2048), (5368709120, 2048), (5452593152, 8)],
        ),
    ]
}

fn volume() -> Volume {
    Volume::open(Arc::new(own_partial_image("fixture-double-v1"))).expect("open fixture")
}

/// ESXi's `vmkfstools -t0`: (file offset, length) of every mapped extent.
fn esxi_data_extents(name: &str) -> Vec<(u64, u64)> {
    let path = repo_root().join(format!("fixtures/vmfs6/fixture-double-v1/truth/{name}.t0.txt"));
    let mut extents = Vec::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let Some((file, _)) = line.split_once("--> [VMFS -- ") else {
            continue;
        };
        let numbers: Vec<u64> = file
            .split(|c: char| !c.is_ascii_digit())
            .filter(|part| !part.is_empty())
            .map(|n| n.parse().unwrap())
            .collect();
        extents.push((numbers[0], numbers[1]));
    }
    extents
}

#[test]
fn the_files_use_double_indirection() {
    let volume = volume();
    for (name, _, size, _) in plans() {
        let stat = volume.stat(&format!("dbl/{name}-flat.vmdk")).unwrap();
        assert_eq!(stat.len, size, "{name}");
        assert_eq!(stat.addressing, AddressingMode::DoubleIndirect, "{name}");
    }
}

#[test]
fn every_written_sector_is_where_it_was_written_and_the_rest_is_zero() {
    let volume = volume();
    for (name, disk_id, size, written) in plans() {
        let disk = volume.open_disk(&format!("dbl/{name}.vmdk")).unwrap();
        assert_eq!(disk.capacity(), size, "{name}");
        let is_written = |sector: u64| {
            written
                .iter()
                .any(|&(first, count)| sector >= first && sector < first + count)
        };
        // Every megabyte that holds data, sector by sector.
        let mut data_sectors = 0u64;
        let mut sector = [0u8; 512];
        for range in disk.data_ranges() {
            assert_eq!(range.start % MIB, 0);
            for number in range.start / 512..range.end / 512 {
                disk.read_exact_at(number * 512, &mut sector).unwrap();
                if is_written(number) {
                    assert_eq!(sector, pattern_sector(disk_id, 1, number), "{name} sector {number}");
                    data_sectors += 1;
                } else {
                    assert!(sector.iter().all(|&byte| byte == 0), "{name} sector {number}");
                }
            }
        }
        // Nothing that was written lies outside the data ranges.
        assert_eq!(
            data_sectors,
            written.iter().map(|&(_, count)| count).sum::<u64>(),
            "{name}"
        );
        // Holes, on both sides of the 2.5 TiB line.
        for offset in [GIB, 2559 * GIB, 2561 * GIB + 512, size - 2 * MIB] {
            disk.read_exact_at(offset, &mut sector).unwrap();
            assert!(sector.iter().all(|&byte| byte == 0), "{name} at {offset}");
        }
    }
}

#[test]
fn the_block_maps_equal_what_esxi_printed() {
    let volume = volume();
    for (name, ..) in plans() {
        let disk = volume.open_disk(&format!("dbl/{name}.vmdk")).unwrap();
        let ours: Vec<(u64, u64)> = disk
            .data_ranges()
            .iter()
            .map(|range| (range.start, range.end - range.start))
            .collect();
        assert_eq!(ours, esxi_data_extents(name), "{name}");
    }
}
