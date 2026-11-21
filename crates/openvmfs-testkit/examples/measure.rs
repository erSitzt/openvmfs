//! Measures what opening a volume and its virtual disks costs: reads from the device, time, memory, and the time of
//! `Disk::map`. No disk data is read.
//!
//! Usage: measure IMAGE [--ranges FILE] DESCRIPTOR...
//!   IMAGE           an image file or device, or `fixture:NAME` for one of the partial images in fixtures/vmfs6
//!   --ranges FILE   also writes every range that was read (OFFSET LENGTH per line), for capturing a large device
//!                   in part (fixtures/recipes/fetch_missing.py)
//! Memory is the growth of the resident set (Linux), so it is rounded to pages and includes allocator overhead.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use openvmfs::{BlockSource, Extent, FileSource, MappedRange, Result, Volume};
use openvmfs_testkit::{own_partial_image, TracingSource};

/// Counts the reads that reach the device.
#[derive(Debug)]
struct Counting {
    inner: Arc<dyn BlockSource>,
    reads: AtomicU64,
    bytes: AtomicU64,
    largest: AtomicU64,
}

impl BlockSource for Counting {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
        self.largest.fetch_max(buf.len() as u64, Ordering::Relaxed);
        self.inner.read_exact_at(offset, buf)
    }
}

impl Counting {
    /// Reads, bytes and largest read since the last call.
    fn take(&self) -> (u64, u64, u64) {
        (
            self.reads.swap(0, Ordering::Relaxed),
            self.bytes.swap(0, Ordering::Relaxed),
            self.largest.swap(0, Ordering::Relaxed),
        )
    }
}

/// Resident set size in bytes, or 0 where `/proc` does not tell.
fn resident() -> u64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|text| {
            text.split_whitespace()
                .nth(1)
                .and_then(|pages| pages.parse::<u64>().ok())
        })
        .map_or(0, |pages| pages * 4096)
}

fn kib(bytes: u64) -> String {
    format!("{:.0} KiB", bytes as f64 / 1024.0)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let ranges_file = args.iter().position(|a| a == "--ranges").map(|at| {
        args.remove(at);
        args.remove(at)
    });
    let image = args.remove(0);
    let source: Arc<dyn BlockSource> = match image.strip_prefix("fixture:") {
        Some(name) => Arc::new(own_partial_image(name)),
        None => Arc::new(FileSource::open(&image).expect("open image")),
    };
    let trace = Arc::new(TracingSource::new(source));
    let device = Arc::new(Counting {
        inner: trace.clone(),
        reads: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
        largest: AtomicU64::new(0),
    });

    let (before, started) = (resident(), Instant::now());
    let volume = Volume::open(device.clone());
    let (reads, bytes, largest) = device.take();
    println!(
        "volume open: {reads} reads, {}, largest {}, {:.1} ms, memory +{}",
        kib(bytes),
        kib(largest),
        started.elapsed().as_secs_f64() * 1000.0,
        kib(resident().saturating_sub(before))
    );
    let volume = match volume {
        Ok(volume) => volume,
        Err(error) => {
            eprintln!("open volume: {error}");
            write_ranges(ranges_file.as_deref(), &trace);
            return;
        }
    };
    let info = volume.info();
    println!("  {} bytes, VMFS {}", info.capacity, info.version);

    for path in &args {
        let (before, started) = (resident(), Instant::now());
        let disk = volume.open_disk(path);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        let (reads, bytes, largest) = device.take();
        let disk = match disk {
            Ok(disk) => disk,
            Err(error) => {
                eprintln!("{path}: {error} (after {reads} reads, {})", kib(bytes));
                continue;
            }
        };
        println!(
            "{path}: {} GiB, {} link(s)\n  open: {reads} reads, {}, largest {}, {elapsed:.1} ms, memory +{}",
            disk.capacity() >> 30,
            disk.chain().len(),
            kib(bytes),
            kib(largest),
            kib(resident().saturating_sub(before))
        );
        println!(
            "  base extents {} ({} bytes each), data ranges {}",
            disk.extents().len(),
            std::mem::size_of::<Extent>(),
            disk.data_ranges().len()
        );

        // The device map in sections of 4 MiB, the way a backup asks for it.
        let section = 4u64 << 20;
        let (mut calls, mut ranges, mut most, mut slowest) = (0u64, 0u64, 0usize, 0f64);
        let started = Instant::now();
        let mut offset = 0;
        while offset < disk.capacity() {
            let end = (offset + section).min(disk.capacity());
            let one = Instant::now();
            let mapped: Vec<MappedRange> = disk.map(offset..end, 4096).expect("map");
            slowest = slowest.max(one.elapsed().as_secs_f64() * 1e6);
            calls += 1;
            ranges += mapped.len() as u64;
            most = most.max(mapped.len());
            offset = end;
        }
        let total = started.elapsed().as_secs_f64();
        let (reads, _, _) = device.take();
        println!(
            "  map in 4 MiB sections: {calls} calls, {ranges} ranges (at most {most} per call), {:.2} µs per call, \
             slowest {slowest:.0} µs, {reads} device reads",
            total * 1e6 / calls as f64
        );
    }
    write_ranges(ranges_file.as_deref(), &trace);
}

fn write_ranges(path: Option<&str>, trace: &TracingSource) {
    if let Some(path) = path {
        let text: String = trace
            .ranges()
            .iter()
            .map(|(offset, len)| format!("{offset} {len}\n"))
            .collect();
        std::fs::write(path, text).expect("write ranges");
    }
}
