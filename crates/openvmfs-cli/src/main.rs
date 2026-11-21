//! `openvmfs`: inspect VMFS6 volumes and read files and virtual disks from them.

// Offsets and lengths here come out of the library, which has validated them. Release builds keep overflow checks
// on, so a mistake stops the program instead of producing a wrong offset.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation
)]

mod rawdev;

use std::io::{Seek, SeekFrom, Write};
use std::process::ExitCode;
use std::sync::Arc;

use openvmfs::{BlockSource, Error, ErrorKind, Extent, ExtentKind, FileKind, FileSource, OpenOptions, Volume};
use sha2::{Digest, Sha256};

const USAGE: &str = "\
openvmfs: read VMFS6 volumes without ESXi (read-only)

Usage:
  openvmfs info <device>                         volume facts
  openvmfs ls <device> [path]                    list a directory
  openvmfs stat <device> <path>                  metadata of a file or directory
  openvmfs cat <device> <path>                   write a file to standard output
  openvmfs fd <device> <path>                    write the raw file descriptor (8192 bytes) to standard output
  openvmfs map <device> <path>                   block map of a file
  openvmfs hash <device> <path>                  SHA-256 of a file
  openvmfs manifest <device> [max-bytes]         every entry with type, size and SHA-256 (files up to max-bytes,
                                                 default 256 MiB), tab separated, for comparing readers
  openvmfs vmdk info <device> <descriptor>       facts about a virtual disk
  openvmfs vmdk map <device> <descriptor>        block map of a virtual disk
  openvmfs vmdk hash [--chunks] <device> <descriptor>
                                                 SHA-256 of the disk, or one hash per MiB that holds data
  openvmfs vmdk export <device> <descriptor> <output>
                                                 write the disk to a raw image (sparse), or to - for stdout

<device> is an image file or a block device: a whole disk with a partition table, or the VMFS partition.
On Windows a raw disk is \\\\.\\PhysicalDriveN (from an elevated prompt); it is opened read-only and may be offline.
For a datastore that spans several devices, give all of them separated by commas, in any order.
Set OPENVMFS_DIRECT=1 to read without the operating system's page cache (O_DIRECT), as a backup of a LUN should.
<descriptor> is the path of the small .vmdk text file inside the datastore, for example vm1/vm1.vmdk.
Exit codes: 0 ok, 1 error, 2 usage, 3 unsupported (the data may be fine, but this tool refuses to guess),
4 in use (a host was writing to the disk when the image was taken; OPENVMFS_ACCEPT_IN_USE=1 reads it anyway).
Experimental: OPENVMFS_REPLAY_JOURNAL=1 reads the volume with its journals replayed, as ESXi would see it after a
crash. Disks that were in use can then be read.
Set OPENVMFS_ACCEPT_UNVERIFIED=1 to read structures that have not been verified against ESXi yet (for testing).";

const CHUNK: usize = 1 << 20;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(CliError::Usage(message)) => {
            eprintln!("openvmfs: {message}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(CliError::Lib(error)) => {
            eprintln!("openvmfs: {error}");
            ExitCode::from(match error.kind() {
                ErrorKind::Unsupported => 3,
                ErrorKind::InUse => 4,
                _ => 1,
            })
        }
        Err(CliError::Io(error)) => {
            // A closed pipe (for example `| head`) is not an error worth reporting.
            if error.kind() == std::io::ErrorKind::BrokenPipe {
                return ExitCode::SUCCESS;
            }
            eprintln!("openvmfs: {error}");
            ExitCode::from(1)
        }
    }
}

enum CliError {
    Usage(String),
    Lib(Error),
    Io(std::io::Error),
}

impl From<Error> for CliError {
    fn from(error: Error) -> Self {
        CliError::Lib(error)
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CliError::Io(error)
    }
}

type CliResult = Result<(), CliError>;

fn usage<T>(message: &str) -> Result<T, CliError> {
    Err(CliError::Usage(message.to_owned()))
}

fn run(args: &[&str]) -> CliResult {
    match args {
        [] | ["-h" | "--help" | "help"] => {
            println!("{USAGE}");
            Ok(())
        }
        ["info", device] => info(device),
        ["ls", device] => ls(device, ""),
        ["ls", device, path] => ls(device, path),
        ["stat", device, path] => stat(device, path),
        ["cat", device, path] => cat(device, path),
        ["fd", device, path] => {
            let raw = open(device)?.read_descriptor(path)?;
            std::io::stdout().lock().write_all(&raw)?;
            Ok(())
        }
        ["map", device, path] => {
            let volume = open(device)?;
            let file = volume.open_file(path)?;
            print_map(&volume, file.extents())
        }
        ["hash", device, path] => {
            let volume = open(device)?;
            let file = volume.open_file(path)?;
            println!("{}  {path}", sha256(&file)?);
            Ok(())
        }
        ["manifest", device] => manifest(device, 256 << 20),
        ["manifest", device, limit] => match limit.parse::<u64>() {
            Ok(limit) => manifest(device, limit),
            Err(_) => usage("max-bytes must be a number"),
        },
        ["vmdk", "info", device, path] => vmdk_info(device, path),
        ["vmdk", "map", device, path] => {
            let volume = open(device)?;
            let disk = volume.open_disk(path)?;
            print_map(&volume, disk.extents())
        }
        ["vmdk", "hash", device, path] => {
            let disk = open(device)?.open_disk(path)?;
            println!("{}  {path}", sha256(&disk)?);
            Ok(())
        }
        ["vmdk", "hash", "--chunks", device, path] => vmdk_hash_chunks(device, path),
        ["vmdk", "export", device, path, output] => vmdk_export(device, path, output),
        _ => usage("unknown command or wrong number of arguments"),
    }
}

/// Opens the volume. `OPENVMFS_ACCEPT_UNVERIFIED=1` switches off the refusal of structures that have not been
/// verified against ESXi; it exists for verification work.
fn open(device: &str) -> Result<Volume, CliError> {
    let accept_unverified = std::env::var_os("OPENVMFS_ACCEPT_UNVERIFIED").is_some_and(|value| value == "1");
    if accept_unverified {
        eprintln!("openvmfs: warning: reading unverified structures; the data may be wrong");
    }
    let options = OpenOptions {
        accept_unverified,
        direct: std::env::var_os("OPENVMFS_DIRECT").is_some_and(|value| value == "1"),
        accept_in_use: std::env::var_os("OPENVMFS_ACCEPT_IN_USE").is_some_and(|value| value == "1"),
        replay_journal: std::env::var_os("OPENVMFS_REPLAY_JOURNAL").is_some_and(|value| value == "1"),
        ..OpenOptions::default()
    };
    // A datastore on several devices is given as a comma-separated list.
    let paths: Vec<&str> = device.split(',').collect();
    if !paths.iter().any(|path| rawdev::is_raw_device(path)) {
        return Ok(Volume::open_paths(&paths, &options)?);
    }
    // Raw disks on Windows need their size asked from the driver and are always read unbuffered.
    let mut devices: Vec<Arc<dyn BlockSource>> = Vec::with_capacity(paths.len());
    for path in paths {
        devices.push(if rawdev::is_raw_device(path) {
            rawdev::open(path)?
        } else if options.direct {
            Arc::new(FileSource::open_direct(path)?)
        } else {
            Arc::new(FileSource::open(path)?)
        });
    }
    Ok(Volume::open_devices(devices, &options)?)
}

fn info(device: &str) -> CliResult {
    let volume = open(device)?;
    let info = volume.info();
    println!("label:              {}", info.label);
    println!("uuid:               {}", info.uuid);
    println!("version:            VMFS {}", info.version);
    println!("capacity:           {} bytes ({})", info.capacity, human(info.capacity));
    println!("file block size:    {}", human(info.file_block_size));
    println!("partition offset:   {:#x}", info.partition_offset);
    println!(
        "lvm volume:         {} (LVM {}.{})",
        info.lvm_volume.uuid, info.lvm_device.major_version, info.lvm_device.minor_version
    );
    for (index, device) in info.lvm_devices.iter().enumerate() {
        if info.lvm_devices.len() == 1 {
            println!("lvm device:         {}", device.device_uuid);
        } else {
            println!(
                "lvm device {}:       {} ({} from volume offset {:#x}{})",
                index.saturating_add(1),
                device.device_uuid,
                human(device.volume_len),
                device.volume_start,
                info.lvm_volume
                    .device_names
                    .get(index)
                    .map(|name| format!(", {name}"))
                    .unwrap_or_default()
            );
        }
    }
    println!(
        "created on:         {} {}",
        info.lvm_device.scsi_vendor, info.lvm_device.scsi_model
    );
    println!(
        "created at:         {} (seconds since 1970)",
        info.descriptor.creation_time
    );
    println!(
        "active heartbeats:  {}{}",
        info.active_heartbeats,
        if info.active_heartbeats == 0 {
            " (no host was holding locks)"
        } else {
            " (hosts were holding locks when this image was taken)"
        }
    );
    match volume.journal_status() {
        Ok(status) if status.replayed => println!(
            "journal:            {} pending transaction(s) in {} journal(s), replayed for this read",
            status.pending_transactions, status.journals
        ),
        Ok(status) if status.pending_transactions > 0 => println!(
            "journal:            {} pending transaction(s) in {} journal(s): metadata was in flux; files that were \
             open for writing lack their newest changes",
            status.pending_transactions, status.journals
        ),
        Ok(status) => println!("journal:            nothing pending in {} journal(s)", status.journals),
        Err(error) => println!("journal:            could not be read ({error})"),
    }
    Ok(())
}

fn kind_letter(kind: FileKind) -> char {
    match kind {
        FileKind::Directory => 'd',
        FileKind::Regular => '-',
        FileKind::Symlink => 'l',
        FileKind::System => 's',
        FileKind::RawDeviceMap => 'r',
        _ => '?',
    }
}

fn ls(device: &str, path: &str) -> CliResult {
    let volume = open(device)?;
    let mut entries = volume.read_dir(path)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = std::io::stdout().lock();
    for entry in entries {
        let full = format!("{}/{}", path.trim_end_matches('/'), entry.name);
        let stat = volume.stat(&full)?;
        writeln!(
            out,
            "{} {:>16} {:>10} {}",
            kind_letter(entry.kind),
            stat.len,
            stat.mtime,
            entry.name
        )?;
    }
    Ok(())
}

fn stat(device: &str, path: &str) -> CliResult {
    let volume = open(device)?;
    let stat = volume.stat(path)?;
    println!("address:     {} ({:#010x})", stat.addr, stat.addr.raw());
    let (device_index, device_offset) = volume.device_location(stat.descriptor_offset)?;
    println!(
        "descriptor:  volume offset {:#x}, device offset {:#x}{}",
        stat.descriptor_offset,
        device_offset,
        if volume.device_count() > 1 {
            format!(" on device {}", device_index.saturating_add(1))
        } else {
            String::new()
        }
    );
    println!(
        "kind:        {:?}{}",
        stat.kind,
        if stat.is_dir { " (directory)" } else { "" }
    );
    println!("length:      {} bytes ({})", stat.len, human(stat.len));
    println!(
        "blocks:      {} allocated, {} never written",
        stat.blocks, stat.tbz_blocks
    );
    println!("addressing:  {:?}", stat.addressing);
    println!(
        "mode:        {:o}  uid {}  gid {}  links {}",
        stat.mode, stat.uid, stat.gid, stat.links
    );
    println!(
        "times:       mtime {}  ctime {}  atime {}",
        stat.mtime, stat.ctime, stat.atime
    );
    println!("generation:  {}", stat.generation);
    if stat.lock == openvmfs::LockMode::Free {
        println!("lock:        free");
    } else {
        println!("lock:        {:?}, held by host {}", stat.lock, stat.lock_owner);
    }
    if stat.kind == FileKind::Symlink {
        println!("target:      {}", volume.read_link(path)?);
    }
    Ok(())
}

fn cat(device: &str, path: &str) -> CliResult {
    let volume = open(device)?;
    let file = volume.open_file(path)?;
    let mut out = std::io::stdout().lock();
    copy(&file, |_, data| out.write_all(data))?;
    out.flush()?;
    Ok(())
}

fn manifest(device: &str, limit: u64) -> CliResult {
    let volume = open(device)?;
    let mut out = std::io::stdout().lock();
    manifest_dir(&volume, "", limit, &mut out)?;
    Ok(out.flush()?)
}

fn manifest_dir(volume: &Volume, dir: &str, limit: u64, out: &mut impl Write) -> CliResult {
    let mut entries = volume.read_dir(dir)?;
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    for entry in entries {
        let path = format!("{dir}/{}", entry.name);
        let stat = volume.stat(&path)?;
        let digest = match stat.kind {
            FileKind::Symlink => hex(&Sha256::digest(volume.read_link(&path)?.as_bytes())),
            FileKind::Regular | FileKind::System if !stat.is_dir && stat.len <= limit => {
                sha256(&volume.open_file(&path)?)?
            }
            _ => "-".to_owned(),
        };
        writeln!(out, "{}\t{}\t{digest}\t{path}", kind_letter(stat.kind), stat.len)?;
        if stat.kind == FileKind::Directory {
            manifest_dir(volume, &path, limit, out)?;
        }
    }
    Ok(())
}

fn print_map(volume: &Volume, extents: &[Extent]) -> CliResult {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "{:>16} {:>16}  {:<6} {:>16} {:>16}",
        "file offset", "length", "kind", "volume offset", "device offset"
    )?;
    for extent in extents {
        let (kind, volume_offset) = match extent.kind {
            ExtentKind::Data { volume_offset } => ("data", Some(volume_offset)),
            ExtentKind::ToBeZeroed { volume_offset } => ("tbz", Some(volume_offset)),
            ExtentKind::Hole => ("hole", None),
        };
        match volume_offset {
            Some(offset) => {
                let (device, device_offset) = volume.device_location(offset)?;
                // On a datastore with several devices, say which one (counted as they were given, from 1).
                let which = if volume.device_count() > 1 {
                    format!("  device {}", device.saturating_add(1))
                } else {
                    String::new()
                };
                writeln!(
                    out,
                    "{:#16x} {:#16x}  {:<6} {:#16x} {:#16x}{which}",
                    extent.offset, extent.len, kind, offset, device_offset
                )?;
            }
            None => writeln!(
                out,
                "{:#16x} {:#16x}  {:<6} {:>16} {:>16}",
                extent.offset, extent.len, kind, "-", "-"
            )?,
        }
    }
    Ok(())
}

fn vmdk_info(device: &str, path: &str) -> CliResult {
    let volume = open(device)?;
    let disk = volume.open_disk(path)?;
    let descriptor = disk.descriptor();
    let data: u64 = disk.data_ranges().iter().map(|range| range.end - range.start).sum();
    println!("capacity:     {} bytes ({})", disk.capacity(), human(disk.capacity()));
    println!("create type:  {}", descriptor.create_type);
    println!("content id:   {:08x}", descriptor.cid);
    println!(
        "thin flag:    {}",
        if descriptor.thin_provisioned() { "yes" } else { "no" }
    );
    println!(
        "version:      {}{}",
        descriptor.version,
        if descriptor.version == 3 {
            " (change tracking enabled)"
        } else {
            ""
        }
    );
    for link in disk.chain() {
        let descriptor = &link.descriptor;
        for extent in &descriptor.extents {
            println!(
                "link:         {} ({}, content id {:08x}): {:?} {:?}",
                link.descriptor_path, descriptor.create_type, descriptor.cid, extent.kind, extent.file_name
            );
        }
    }
    println!(
        "data:         {} bytes ({}) in {} ranges",
        data,
        human(data),
        disk.data_ranges().len()
    );
    Ok(())
}

fn vmdk_hash_chunks(device: &str, path: &str) -> CliResult {
    let disk = open(device)?.open_disk(path)?;
    let mut out = std::io::stdout().lock();
    let mut buf = vec![0u8; CHUNK];
    for range in disk.data_ranges() {
        // Hash every MiB-aligned chunk that touches a data range.
        let mut offset = range.start - range.start % CHUNK as u64;
        while offset < range.end {
            let len = (CHUNK as u64).min(disk.capacity() - offset) as usize;
            let chunk = &mut buf[..len];
            disk.read_exact_at(offset, chunk)?;
            writeln!(out, "{offset:#014x} {}", hex(&Sha256::digest(&*chunk)))?;
            offset += CHUNK as u64;
        }
    }
    Ok(())
}

fn vmdk_export(device: &str, path: &str, output: &str) -> CliResult {
    let disk = open(device)?.open_disk(path)?;
    if output == "-" {
        let mut out = std::io::stdout().lock();
        copy(&disk, |_, data| out.write_all(data))?;
        return Ok(out.flush()?);
    }
    let mut file = std::fs::File::create(output)?;
    let mut buf = vec![0u8; CHUNK];
    for range in disk.data_ranges() {
        let mut offset = range.start;
        while offset < range.end {
            let len = (CHUNK as u64).min(range.end - offset) as usize;
            let chunk = &mut buf[..len];
            disk.read_exact_at(offset, chunk)?;
            file.seek(SeekFrom::Start(offset))?;
            file.write_all(chunk)?;
            offset += len as u64;
        }
    }
    // Everything that was not written stays a hole and reads as zeros.
    file.set_len(disk.capacity())?;
    file.sync_all()?;
    Ok(())
}

/// Feeds the whole source to `sink` in chunks.
fn copy(source: &dyn BlockSource, mut sink: impl FnMut(u64, &[u8]) -> std::io::Result<()>) -> CliResult {
    let mut buf = vec![0u8; CHUNK];
    let mut offset = 0u64;
    while offset < source.len() {
        let len = (CHUNK as u64).min(source.len() - offset) as usize;
        let chunk = &mut buf[..len];
        source.read_exact_at(offset, chunk)?;
        sink(offset, chunk)?;
        offset += len as u64;
    }
    Ok(())
}

fn sha256(source: &dyn BlockSource) -> Result<String, CliError> {
    let mut hasher = Sha256::new();
    copy(source, |_, data| {
        hasher.update(data);
        Ok(())
    })?;
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}
