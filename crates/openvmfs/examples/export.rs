//! Copies a virtual disk into a raw image file, reading only the ranges that hold data.
//!
//! Usage: cargo run --release --example export -- DEVICE DESCRIPTOR OUTPUT
//! Example: export /dev/mapper/snapshot-lun vm1/vm1.vmdk vm1.raw

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

use openvmfs::{BlockSource, Volume};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [device, descriptor, output] = args.as_slice() else {
        return Err("usage: export DEVICE DESCRIPTOR OUTPUT".into());
    };
    let volume = Volume::open_path(device)?;
    // The descriptor is the small .vmdk text file. For a disk with snapshots, name the level you want.
    let disk = volume.open_disk(descriptor)?;

    let mut out = File::create(output)?;
    out.set_len(disk.capacity())?;
    let mut buffer = vec![0u8; 4 << 20];
    // Everything outside the data ranges reads as zeros, so it is skipped and the output stays sparse.
    for range in disk.data_ranges() {
        let mut offset = range.start;
        while offset < range.end {
            let wanted = usize::try_from(range.end.saturating_sub(offset))
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let Some(chunk) = buffer.get_mut(..wanted) else { break };
            disk.read_exact_at(offset, chunk)?;
            out.seek(SeekFrom::Start(offset))?;
            out.write_all(chunk)?;
            offset = offset.saturating_add(wanted as u64);
        }
    }
    println!("{} bytes written to {output}", disk.capacity());
    Ok(())
}
