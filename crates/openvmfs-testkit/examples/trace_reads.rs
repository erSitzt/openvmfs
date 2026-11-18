//! Prints which parts of a device OpenVmfs reads to open a volume and hash the given virtual disks.
//!
//! Used when a large, mostly empty device is captured only in part: every range printed here must be inside the
//! captured ranges, otherwise the reader saw zeros where the device has data.
//!
//! Usage: trace_reads IMAGE [--unverified] [--in-use] (DESCRIPTOR | dir:PATH | file:PATH)...
//! Output: OFFSET LENGTH per line, merged.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]

use std::sync::Arc;

use openvmfs::{BlockSource, FileSource, OpenOptions, Volume};
use openvmfs_testkit::TracingSource;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let accept_unverified = args.iter().any(|a| a == "--unverified");
    let accept_in_use = args.iter().any(|a| a == "--in-use");
    args.retain(|a| a != "--unverified" && a != "--in-use");
    let image = args.remove(0);
    let trace = Arc::new(TracingSource::new(Arc::new(
        FileSource::open(&image).expect("open image"),
    )));
    let options = OpenOptions {
        accept_unverified,
        accept_in_use,
        ..OpenOptions::default()
    };
    // When the image lacks something the volume needs, the reads made so far say what to fetch next.
    let volume = match Volume::open_with(trace.clone(), &options) {
        Ok(volume) => volume,
        Err(error) => {
            eprintln!("open volume: {error}");
            for (offset, len) in trace.ranges() {
                println!("{offset} {len}");
            }
            return;
        }
    };
    let mut buf = vec![0u8; 1 << 20];
    for arg in &args {
        // `dir:PATH` lists a directory, `file:PATH` reads a file, anything else is a disk descriptor.
        let result = if let Some(path) = arg.strip_prefix("dir:") {
            volume.read_dir(path).map(|_| ())
        } else if let Some(path) = arg.strip_prefix("file:") {
            volume
                .open_file(path)
                .and_then(|file| file.read_all(1 << 30))
                .map(|_| ())
        } else {
            volume.open_disk(arg).and_then(|disk| {
                for range in disk.data_ranges() {
                    let mut offset = range.start;
                    while offset < range.end {
                        let len = (buf.len() as u64).min(range.end - offset) as usize;
                        disk.read_exact_at(offset, &mut buf[..len])?;
                        offset += len as u64;
                    }
                }
                Ok(())
            })
        };
        if let Err(error) = result {
            eprintln!("{arg}: {error}");
        }
    }
    for (offset, len) in trace.ranges() {
        println!("{offset} {len}");
    }
}
