//! Damaged metadata: whatever a device holds, the library ends with data or an error. It never panics, never hangs
//! and never reads outside the device.
//!
//! Each test first records which bytes a clean run reads, then repeats the run many times with a few of those bytes
//! changed. (Wrong data cannot always be noticed: VMFS metadata carries no checksums.)

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

use openvmfs::{BlockSource, ErrorKind, FileSource, OpenOptions, Volume};
use openvmfs_testkit::{own_image, own_partial_image, MutatedSource, PartialSource, Rng, TracingSource};

const MIB: u64 = 1 << 20;

/// Opens each disk and reads the start of its data ranges and a few fixed places.
fn read_disks(volume: &Volume, disks: &[&str]) -> openvmfs::Result<()> {
    let mut buf = vec![0u8; 8192];
    for path in disks {
        let disk = volume.open_disk(path)?;
        for range in disk.data_ranges().iter().take(6) {
            let len = (buf.len() as u64).min(range.end - range.start) as usize;
            disk.read_exact_at(range.start, &mut buf[..len])?;
        }
        for offset in [0, disk.capacity() / 2, disk.capacity() - 8192] {
            disk.read_exact_at(offset, &mut buf)?;
        }
    }
    Ok(())
}

/// Runs `exercise` on the clean devices, then `rounds` times with one to three damaged bytes among those that the
/// clean run read. Returns how many damaged runs ended with an error.
fn damage(
    devices: Vec<Arc<dyn BlockSource>>,
    rounds: u32,
    seed: u64,
    exercise: impl Fn(&Volume) -> openvmfs::Result<()>,
) -> u32 {
    let open = |devices: Vec<Arc<dyn BlockSource>>| Volume::open_devices(devices, &OpenOptions::default());
    let traces: Vec<Arc<TracingSource>> = devices
        .iter()
        .map(|device| Arc::new(TracingSource::new(device.clone())))
        .collect();
    exercise(
        &open(
            traces
                .iter()
                .map(|trace| trace.clone() as Arc<dyn BlockSource>)
                .collect(),
        )
        .unwrap(),
    )
    .unwrap();
    // (device, offset, length) of everything that was read, without large data reads.
    let ranges: Vec<(usize, u64, u64)> = traces
        .iter()
        .enumerate()
        .flat_map(|(device, trace)| {
            trace
                .ranges()
                .into_iter()
                .map(move |(offset, len)| (device, offset, len))
        })
        .filter(|(_, _, len)| *len <= 2 * MIB)
        .collect();
    assert!(!ranges.is_empty());

    let mut rng = Rng::new(seed);
    let mut refused = 0;
    for round in 0..rounds {
        let mut patches: Vec<Vec<(u64, u8)>> = vec![Vec::new(); devices.len()];
        for _ in 0..1 + rng.below(3) {
            let (device, start, len) = ranges[rng.below(ranges.len() as u64) as usize];
            // Most of what is read is zero padding. Prefer a byte that holds something.
            let mut at = start + rng.below(len);
            let mut window = vec![0u8; ((start + len - at).min(4096)) as usize];
            devices[device].read_exact_at(at, &mut window).unwrap();
            let used: Vec<usize> = window
                .iter()
                .enumerate()
                .filter(|(_, byte)| **byte != 0)
                .map(|(i, _)| i)
                .collect();
            if !used.is_empty() {
                at += used[rng.below(used.len() as u64) as usize] as u64;
            }
            patches[device].push((at, rng.next_u64() as u8));
        }
        let damaged: Vec<Arc<dyn BlockSource>> = devices
            .iter()
            .zip(&patches)
            .map(|(device, patches)| {
                Arc::new(MutatedSource::new(device.clone(), patches.clone())) as Arc<dyn BlockSource>
            })
            .collect();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            open(damaged).and_then(|volume| exercise(&volume))
        }));
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                // A read outside a device is caught before it reaches the device and is not an I/O error.
                assert_ne!(error.kind(), ErrorKind::Io, "round {round} {patches:?}: {error}");
                refused += 1;
            }
            Err(_) => panic!("round {round}: the library panicked on damaged metadata {patches:?}"),
        }
    }
    eprintln!("{refused} of {rounds} damaged runs were refused, the rest read without error");
    refused
}

#[test]
fn damaged_snapshot_chains_never_panic() {
    let device: Arc<dyn BlockSource> = Arc::new(FileSource::open(own_image("fixture-chain-v1")).unwrap());
    let refused = damage(vec![device], 6000, 0x5E5_BA5E, |volume| {
        read_disks(
            volume,
            &["guest/grow-000001.vmdk", "guest/thin-g-000001.vmdk", "guest/grow.vmdk"],
        )
    });
    assert!(refused > 0);
}

#[test]
fn damaged_devices_of_a_spanned_datastore_never_panic() {
    let devices: Vec<Arc<dyn BlockSource>> = ["fixture-span-v1-dev1", "fixture-span-v1-dev2"]
        .iter()
        .map(|name| Arc::new(FileSource::open(own_image(name)).unwrap()) as Arc<dyn BlockSource>)
        .collect();
    let refused = damage(devices, 6000, 0x5BA2, |volume| {
        volume.read_dir("")?;
        volume.read_dir("d")?;
        read_disks(volume, &["d/lazy.vmdk", "d/thin.vmdk"])
    });
    assert!(refused > 0);
}

/// A partial fixture as a whole device: what was not captured reads as zeros, so that a damaged address that points
/// anywhere inside the device can be followed.
#[derive(Debug)]
struct ZeroFilled(PartialSource);

impl BlockSource for ZeroFilled {
    fn len(&self) -> u64 {
        self.0.len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> openvmfs::Result<()> {
        let inside = offset
            .checked_add(buf.len() as u64)
            .is_some_and(|end| end <= self.0.len());
        if inside && !self.0.covers(offset, buf.len() as u64) {
            buf.fill(0);
            return Ok(());
        }
        self.0.read_exact_at(offset, buf)
    }
}

#[test]
fn damaged_double_indirect_files_never_panic() {
    let device: Arc<dyn BlockSource> = Arc::new(ZeroFilled(own_partial_image("fixture-double-v1")));
    let refused = damage(vec![device], 6000, 0xD0B1E, |volume| {
        volume.read_dir("dbl")?;
        read_disks(volume, &["dbl/huge.vmdk", "dbl/edge.vmdk"])
    });
    assert!(refused > 0);
}
