//! Reads the public VMFS 6.82 sample images (written by ESXi, published with dissect.vmfs) through the library.
//!
//! The expectations are facts about the images: file names and sizes, content that describes itself, and SHA-256
//! values that an independent reader (dissect.vmfs, run in the oracle container) confirmed on 2026-11-18.
//! The tests skip when the images have not been downloaded (`fixtures/fetch-external.sh`).

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

use std::sync::Arc;

use openvmfs::{BlockSource, ErrorKind, ExtentKind, FileKind, FileSource, Volume};
use openvmfs_testkit::{external_image, sha256_hex, sha256_of, MutatedSource, Rng, TracingSource};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

fn open(name: &str) -> Option<Volume> {
    let image = external_image(name)?;
    Some(Volume::open_path(image).expect("open sample image"))
}

#[test]
fn volume_facts() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    let info = volume.info();
    assert_eq!(info.label, "vmfs6");
    assert_eq!(info.version, "6.82");
    assert_eq!(info.uuid.to_string(), "68626f2d-90e23158-8a2e-bc2411b17e28");
    assert_eq!(info.capacity, 2 * GIB);
    assert_eq!(info.file_block_size, MIB);
    assert_eq!(info.partition_offset, 0);
    assert_eq!(info.lvm_volume.uuid.to_string(), "68626f2d-86473108-d4e1-bc2411b17e28");
    // One host was holding a lock on the datastore when the image was taken.
    assert_eq!(info.active_heartbeats, 1);
}

#[test]
fn root_directory() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    let mut names: Vec<String> = volume
        .read_dir("/")
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            ".fbb.sf",
            ".fdc.sf",
            ".jbc.sf",
            ".pb2.sf",
            ".pbc.sf",
            ".sbc.sf",
            ".sdd.sf",
            ".unmap.sf",
            ".vh.sf",
            "big",
            "bigger",
            "collision",
            "directory",
            "large-flat.vmdk",
            "large.vmdk",
            "normal-flat.vmdk",
            "normal.vmdk",
            "rdm-rdm.vmdk",
            "rdm.vmdk",
            "small",
            "symlink",
        ]
    );
    assert_eq!(volume.stat("directory").unwrap().kind, FileKind::Directory);
    assert_eq!(volume.stat(".sdd.sf").unwrap().kind, FileKind::System);
    assert!(volume.stat(".sdd.sf").unwrap().is_dir);
    assert_eq!(volume.stat("rdm-rdm.vmdk").unwrap().kind, FileKind::RawDeviceMap);
}

#[test]
fn resident_file_and_symlink() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    let small = volume.open_file("small").unwrap();
    assert_eq!(small.read_all(MIB).unwrap(), b"tiny\n");
    assert_eq!(volume.read_link("symlink").unwrap(), "/vmfs/volumes/vmfs6/small");
    assert_eq!(volume.open_file("symlink").unwrap_err().kind(), ErrorKind::InvalidInput);
}

#[test]
fn directory_stored_in_sub_blocks() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    let entries = volume.read_dir("directory").unwrap();
    assert_eq!(entries.len(), 100);
    for number in [1, 42, 100] {
        let file = volume.open_file(&format!("directory/file{number}")).unwrap();
        assert_eq!(file.read_all(MIB).unwrap(), format!("File {number}\n").as_bytes());
    }
    // Names with spaces, non-ASCII characters and emoji; all files are empty.
    let collision = volume.read_dir("/collision/").unwrap();
    assert_eq!(collision.len(), 19);
    assert!(collision
        .iter()
        .any(|entry| entry.name == "En groetjes aan je tante🤤🫰🍆💦😍"));
}

#[test]
fn file_blocks_hold_the_right_data() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    // `bigger` is 128 MiB; MiB number n is filled with the byte n. A reader that maps blocks wrongly cannot pass.
    let bigger = volume.open_file("bigger").unwrap();
    assert_eq!(bigger.len(), 128 * MIB);
    let mut buf = vec![0u8; MIB as usize];
    for n in 0..128u64 {
        bigger.read_exact_at(n * MIB, &mut buf).unwrap();
        assert!(
            buf.iter().all(|&b| u64::from(b) == n),
            "MiB {n} of `bigger` holds foreign data"
        );
    }
    // A read that crosses a block boundary.
    let mut cross = [0u8; 4];
    bigger.read_exact_at(5 * MIB - 2, &mut cross).unwrap();
    assert_eq!(cross, [4, 4, 5, 5]);

    let big = volume.open_file("big").unwrap();
    assert_eq!(big.len(), MIB + 1);
    assert_eq!(
        sha256_of(&big),
        "5053579a1aa6aa9c5be1e571efd58862f18bb57acd4625d7ad442a050eb8d0f9"
    );
    assert_eq!(
        sha256_of(&bigger),
        "ec5223559dc61ecef215c97e5b6c1a0fad4cab8530aed3de1608c6cd47892ef9"
    );
}

#[test]
fn system_files_read_like_any_file() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    // The pointer-block file of this volume is addressed directly; the sub-block file through `.pb2.sf`.
    assert_eq!(
        sha256_of(&volume.open_file(".pb2.sf").unwrap()),
        "81c34914228b501a6bbfd09179b436f7ec3d0306f50179514ab678b281b81b61"
    );
    assert_eq!(
        sha256_of(&volume.open_file(".fdc.sf").unwrap()),
        "7680df4678e5492b3b41e5f3a315a7652faa45e81d59ed4d476bc9246ef2357c"
    );
}

#[test]
fn thin_disk_with_single_indirection() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    let disk = volume.open_disk("normal.vmdk").unwrap();
    assert_eq!(disk.capacity(), 69 * GIB);
    assert_eq!(disk.descriptor().create_type, "vmfs");
    assert!(disk.descriptor().thin_provisioned());
    // One MiB was written at 42 GiB; everything else is unallocated.
    let written = 42 * GIB;
    assert_eq!(disk.data_ranges(), &[written..written + MIB]);
    assert_eq!(disk.extents().len(), 3);
    assert_eq!(disk.extents()[0].kind, ExtentKind::Hole);
    assert!(matches!(disk.extents()[1].kind, ExtentKind::Data { .. }));

    let mut buf = vec![0u8; MIB as usize];
    disk.read_exact_at(written, &mut buf).unwrap();
    assert_eq!(
        sha256_hex(&buf),
        "ce6475b1fc22ad8543d2aee58ef77aa19eb5d208c372f4ee69885683e0213a08"
    );
    for hole in [0, written - MIB, written + MIB, disk.capacity() - MIB] {
        disk.read_exact_at(hole, &mut buf).unwrap();
        assert!(
            buf.iter().all(|&b| b == 0),
            "unallocated range at {hole:#x} must read as zeros"
        );
    }
    let mut beyond = [0u8; 1];
    assert_eq!(
        disk.read_exact_at(disk.capacity(), &mut beyond).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
}

#[test]
fn thin_disk_with_double_indirection() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    let disk = volume.open_disk("/large.vmdk").unwrap();
    assert_eq!(disk.capacity(), 42 * 1024 * GIB);
    let written = 117 * GIB;
    assert_eq!(disk.data_ranges(), &[written..written + MIB]);
    let mut buf = vec![0u8; MIB as usize];
    disk.read_exact_at(written, &mut buf).unwrap();
    assert_eq!(
        sha256_hex(&buf),
        "ce6475b1fc22ad8543d2aee58ef77aa19eb5d208c372f4ee69885683e0213a08"
    );
    disk.read_exact_at(40 * 1024 * GIB, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn empty_thin_disk() {
    let Some(volume) = open("vmfs/zero6") else { return };
    assert_eq!(volume.info().active_heartbeats, 0);
    let disk = volume.open_disk("empty.vmdk").unwrap();
    assert_eq!(disk.capacity(), 100 * GIB);
    assert!(disk.data_ranges().is_empty());
    let mut buf = vec![0xAAu8; 4096];
    disk.read_exact_at(50 * GIB, &mut buf).unwrap();
    assert!(buf.iter().all(|&b| b == 0));
}

#[test]
fn what_cannot_be_read_is_refused() {
    let Some(volume) = open("vmfs/vmfs6") else { return };
    // A raw device mapping: the descriptor says so, and the mapping file itself is refused too.
    assert_eq!(volume.open_disk("rdm.vmdk").unwrap_err().kind(), ErrorKind::Unsupported);
    assert_eq!(
        volume.open_file("rdm-rdm.vmdk").unwrap_err().kind(),
        ErrorKind::Unsupported
    );
    // Not a descriptor.
    assert_eq!(
        volume.open_disk("normal-flat.vmdk").unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(volume.open_disk("small").unwrap_err().kind(), ErrorKind::Corrupt);
    assert_eq!(
        volume.open_disk("missing.vmdk").unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        volume.open_file("directory").unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(volume.read_dir("small").unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(volume.stat("../x").unwrap_err().kind(), ErrorKind::InvalidInput);

    // VMFS-L (the ESXi system volume flavour) is not a datastore.
    if let Some(image) = external_image("vmfs/vmfs6l") {
        assert_eq!(Volume::open_path(image).unwrap_err().kind(), ErrorKind::Unsupported);
    }
}

#[test]
fn volume_with_resource_optimised_layout() {
    let Some(volume) = open("vmfs/vmfs6-resource-optimized") else {
        return;
    };
    assert_eq!(volume.info().label, "vmfs6-resource-optimized");
    assert_eq!(volume.read_dir("").unwrap().len(), 9);
    assert_eq!(volume.open_file(".pbc.sf").unwrap().len(), 65536);
}

/// Everything the tests above read, in one function, for the robustness test.
fn exercise(volume: &Volume) -> openvmfs::Result<()> {
    for entry in volume.read_dir("")? {
        let _ = volume.stat(&entry.name)?;
    }
    volume.read_dir("directory")?;
    volume.open_file("directory/file7")?.read_all(MIB)?;
    let mut buf = vec![0u8; 4096];
    volume.open_file("bigger")?.read_exact_at(100 * MIB, &mut buf)?;
    for disk in ["normal.vmdk", "large.vmdk"] {
        let disk = volume.open_disk(disk)?;
        for range in disk.data_ranges() {
            disk.read_exact_at(range.start, &mut buf)?;
        }
    }
    Ok(())
}

#[test]
fn damaged_metadata_never_panics_or_reads_outside_the_device() {
    let Some(image) = external_image("vmfs/vmfs6") else {
        return;
    };
    let device: Arc<dyn BlockSource> = Arc::new(FileSource::open(image).unwrap());

    // Record which bytes a clean run reads; only damage there can have an effect.
    let trace = Arc::new(TracingSource::new(device.clone()));
    exercise(&Volume::open(trace.clone()).unwrap()).unwrap();
    let ranges: Vec<(u64, u64)> = trace.ranges().into_iter().filter(|(_, len)| *len <= 2 * MIB).collect();
    assert!(!ranges.is_empty());

    let mut rng = Rng::new(0x0BAD_5EED);
    let mut refused = 0;
    let rounds = 400;
    for round in 0..rounds {
        // One to three damaged bytes somewhere in the metadata that was read.
        let patches: Vec<(u64, u8)> = (0..1 + rng.below(3))
            .map(|_| {
                let (start, len) = ranges[rng.below(ranges.len() as u64) as usize];
                (start + rng.below(len), rng.next_u64() as u8)
            })
            .collect();
        let damaged: Arc<dyn BlockSource> = Arc::new(MutatedSource::new(device.clone(), patches.clone()));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            Volume::open(damaged).and_then(|volume| exercise(&volume))
        }));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                // Out-of-range reads are caught by the source and surface as errors, never as I/O on other data.
                assert_ne!(error.kind(), ErrorKind::Io, "round {round} {patches:?}: {error}");
                refused += 1;
            }
            Err(_) => panic!("round {round}: the library panicked on damaged metadata {patches:?}"),
        }
    }
    // Most single-byte damage hits padding or unused fields; some of it must be noticed.
    assert!(refused > 0, "none of {rounds} damaged images was refused");
    eprintln!("{refused} of {rounds} damaged images were refused, the rest read without error");
}
