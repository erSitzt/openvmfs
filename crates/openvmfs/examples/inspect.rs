//! Shows what is on a VMFS6 datastore: volume facts, the directories of the first level, and every virtual disk
//! with its size and how much of it holds data.
//!
//! Usage: cargo run --release --example inspect -- DEVICE[,DEVICE...]

use openvmfs::{ErrorKind, FileKind, OpenOptions, Volume};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let devices: Vec<String> = match std::env::args().nth(1) {
        Some(list) => list.split(',').map(str::to_owned).collect(),
        None => return Err("usage: inspect DEVICE[,DEVICE...]".into()),
    };
    // A datastore on several devices needs all of them; one device is the common case.
    let volume = Volume::open_paths(&devices, &OpenOptions::default())?;

    let info = volume.info();
    println!(
        "datastore {:?}, VMFS {}, {} bytes",
        info.label, info.version, info.capacity
    );
    println!(
        "hosts holding locks when the image was taken: {}",
        info.active_heartbeats
    );

    for directory in volume.read_dir("")? {
        if directory.kind != FileKind::Directory {
            continue;
        }
        for entry in volume.read_dir(&directory.name)? {
            // A descriptor is a small text file; the big files next to it are its extents.
            let is_descriptor = entry.name.ends_with(".vmdk")
                && !entry.name.ends_with("-flat.vmdk")
                && !entry.name.ends_with("-sesparse.vmdk")
                && !entry.name.ends_with("-ctk.vmdk");
            if !is_descriptor {
                continue;
            }
            let path = format!("{}/{}", directory.name, entry.name);
            match volume.open_disk(&path) {
                Ok(disk) => {
                    let data: u64 = disk
                        .data_ranges()
                        .iter()
                        .map(|range| range.end.saturating_sub(range.start))
                        .sum();
                    println!(
                        "{path}: {} bytes, {data} bytes of data, {} link(s)",
                        disk.capacity(),
                        disk.chain().len()
                    );
                }
                // The disk may be fine; OpenVmfs just will not read it. A caller with another way to read falls back.
                Err(error) if error.kind() == ErrorKind::Unsupported => println!("{path}: refused: {error}"),
                // A host was writing to it when the image was taken.
                Err(error) if error.kind() == ErrorKind::InUse => println!("{path}: in use: {error}"),
                Err(error) => return Err(error.into()),
            }
        }
    }
    Ok(())
}
