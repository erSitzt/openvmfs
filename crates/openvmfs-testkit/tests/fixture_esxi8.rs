//! Reads `fixtures/vmfs6/fixture-basic-esxi8-v1`: the basic test datastore (`fixtures/recipes/make_basic.sh`) made by
//! ESXi 8.0 U3. Everything else in `fixtures/vmfs6` comes from ESXi 7.0.3.

// Test code: assertions may panic, and sizes are small.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation
)]

use openvmfs::{BlockSource, Volume};
use openvmfs_testkit::{own_image, repo_root, sha256_hex, ExpectedDisk, JUNK_MAGIC};

const MIB: u64 = 1 << 20;

fn volume() -> Volume {
    Volume::open_path(own_image("fixture-basic-esxi8-v1")).expect("open fixture")
}

fn esxi_sha256(path: &str) -> String {
    let truth =
        std::fs::read_to_string(repo_root().join("fixtures/vmfs6/fixture-basic-esxi8-v1/truth/sha256.txt")).unwrap();
    truth
        .lines()
        .find_map(|line| line.strip_suffix(path).map(|hash| hash.trim().to_owned()))
        .unwrap_or_else(|| panic!("no ESXi hash recorded for {path}"))
}

#[test]
fn esxi_8_formats_the_same_version() {
    let info = volume().info();
    assert_eq!(info.version, "6.82");
    assert_eq!(info.label, "fz8");
}

#[test]
fn disks_read_what_was_written_and_what_esxi_8_read() {
    let volume = volume();
    // What `make_basic.sh` asked `pattern.py write` to write, as (first sector, count).
    let plans: [(&str, u32, u64, Vec<(u64, u64)>); 4] = [
        (
            "thin",
            1,
            1024 * MIB,
            vec![(0, 2048), (10240, 2048), (614400, 4096), (2095104, 2048)],
        ),
        ("lazy", 2, 300 * MIB, vec![(0, 8), (204800, 2048), (614392, 8)]),
        ("eager", 3, 64 * MIB, vec![(0, 131072)]),
        ("big", 4, 1100 * MIB, vec![(0, 2048), (1048576, 2048), (2250752, 2048)]),
    ];
    for (name, disk_id, size, written) in plans {
        let disk = volume.open_disk(&format!("pat/{name}.vmdk")).unwrap();
        let ours = ExpectedDisk {
            disk_id,
            generation: 1,
            size,
            written,
        }
        .assert_matches(&disk, name);
        assert_eq!(ours, esxi_sha256(&format!("pat/{name}-flat.vmdk")), "{name}");
    }
}

#[test]
fn junk_under_the_lazy_disk_is_on_the_device_but_not_in_what_is_read() {
    let volume = volume();
    let flat = volume.open_file("pat/lazy-flat.vmdk").unwrap();
    assert!(flat.stat().tbz_blocks > 0);
    // A megabyte that was never written: zeros through the reader, junk on the volume underneath.
    let mut through = [0xAAu8; 512];
    flat.read_exact_at(50 * MIB, &mut through).unwrap();
    assert!(through.iter().all(|&byte| byte == 0));
    let below = flat
        .extents()
        .iter()
        .find_map(|extent| match extent.kind {
            openvmfs::ExtentKind::ToBeZeroed { volume_offset } => Some(volume_offset),
            _ => None,
        })
        .unwrap();
    let mut raw = [0u8; 512];
    volume.logical_volume().read_exact_at(below, &mut raw).unwrap();
    assert!(raw.starts_with(JUNK_MAGIC));
}

#[test]
fn files_read_like_on_esxi_8() {
    let volume = volume();
    for path in [
        "files/tiny.txt",
        "files/subblock.bin",
        "files/blocks.bin",
        "files/many/f0000",
        "files/many/f0299",
    ] {
        let content = volume.open_file(path).unwrap().read_all(8 * MIB).unwrap();
        assert_eq!(sha256_hex(&content), esxi_sha256(path), "{path}");
    }
    assert_eq!(volume.read_dir("files/many").unwrap().len(), 300);
    assert_eq!(volume.read_link("files/link").unwrap(), "tiny.txt");
}
