//! Damages the metadata of the fixtures at random, for as long as you like, and reports every run in which the
//! library panicked, read outside a device or took too long.
//!
//! This is the long-running sibling of `tests/robustness.rs`: more fixtures (also journal replay, zero grains, the
//! second pointer block and grown datastores), damage to whole fields and not only single bytes, any seed and any
//! number of rounds. It needs no fuzzing toolchain.
//!
//! Usage: soak [--seed N] [--rounds N] [CASE...]        (without CASE: all of them; `soak --list` names them)
//! Exit status: 0 when no run failed, 1 otherwise. A failing run prints its case, seed, round and the changed bytes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::type_complexity
)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openvmfs::{BlockSource, ErrorKind, FileSource, OpenOptions, Volume};
use openvmfs_testkit::{own_image, own_partial_image, MutatedSource, PartialSource, Rng, TracingSource};

const MIB: u64 = 1 << 20;
/// A run that takes longer than this is reported; the clean runs take milliseconds.
const SLOW: Duration = Duration::from_secs(5);
/// A run that takes longer than this is taken for a hang and ends the program.
const HANG: Duration = Duration::from_secs(120);

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

fn whole(name: &str) -> Arc<dyn BlockSource> {
    Arc::new(FileSource::open(own_image(name)).unwrap())
}

fn partial(name: &str) -> Arc<dyn BlockSource> {
    Arc::new(ZeroFilled(own_partial_image(name)))
}

/// Opens each disk, asks for its map (`map` is not offered after a journal replay) and reads the start of its data
/// ranges and a few fixed places.
fn read_disks(volume: &Volume, disks: &[&str]) -> openvmfs::Result<()> {
    exercise_disks(volume, disks, true)
}

fn exercise_disks(volume: &Volume, disks: &[&str], with_map: bool) -> openvmfs::Result<()> {
    let mut buf = vec![0u8; 8192];
    for path in disks {
        let disk = volume.open_disk(path)?;
        for range in disk.data_ranges().iter().take(6) {
            let len = (buf.len() as u64).min(range.end - range.start) as usize;
            disk.read_exact_at(range.start, &mut buf[..len])?;
        }
        let capacity = disk.capacity();
        for offset in [0, capacity / 2, capacity.saturating_sub(8192)] {
            let len = (buf.len() as u64).min(capacity - offset) as usize;
            disk.read_exact_at(offset, &mut buf[..len])?;
            if with_map {
                disk.map(offset..(offset + 4 * MIB).min(capacity), 4096)?;
            }
        }
    }
    Ok(())
}

/// Reads the start, the middle and the end of each file.
fn read_files(volume: &Volume, files: &[&str]) -> openvmfs::Result<()> {
    let mut buf = vec![0u8; 8192];
    for path in files {
        let file = volume.open_file(path)?;
        let len = file.len();
        for offset in [0, len / 2, len.saturating_sub(8192)] {
            let n = (buf.len() as u64).min(len - offset) as usize;
            file.read_exact_at(offset, &mut buf[..n])?;
        }
    }
    Ok(())
}

struct Case {
    name: &'static str,
    devices: fn() -> Vec<Arc<dyn BlockSource>>,
    options: fn() -> OpenOptions,
    exercise: fn(&Volume) -> openvmfs::Result<()>,
}

fn replay() -> OpenOptions {
    OpenOptions {
        replay_journal: true,
        ..OpenOptions::default()
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "chain",
            devices: || vec![whole("fixture-chain-v1")],
            options: OpenOptions::default,
            exercise: |v| {
                read_disks(
                    v,
                    &["guest/grow-000001.vmdk", "guest/thin-g-000001.vmdk", "guest/grow.vmdk"],
                )
            },
        },
        Case {
            name: "span",
            devices: || vec![whole("fixture-span-v1-dev1"), whole("fixture-span-v1-dev2")],
            options: OpenOptions::default,
            exercise: |v| {
                v.read_dir("")?;
                v.read_dir("d")?;
                read_disks(v, &["d/lazy.vmdk", "d/thin.vmdk"])
            },
        },
        Case {
            name: "grown",
            devices: || {
                (1..=3)
                    .map(|n| whole(&format!("fixture-span3-esxi8-v1-dev{n}")))
                    .collect()
            },
            options: OpenOptions::default,
            exercise: |v| {
                v.read_dir("pat")?;
                read_disks(v, &["pat/l2.vmdk", "pat/l4.vmdk", "pat/t.vmdk"])
            },
        },
        Case {
            name: "double",
            devices: || vec![partial("fixture-double-v1")],
            options: OpenOptions::default,
            exercise: |v| {
                v.read_dir("dbl")?;
                read_disks(v, &["dbl/huge.vmdk", "dbl/edge.vmdk"])
            },
        },
        Case {
            name: "merge",
            devices: || vec![partial("fixture-merge-esxi8-v1")],
            options: OpenOptions::default,
            exercise: |v| read_disks(v, &["misc/pv-000006.vmdk"]),
        },
        Case {
            name: "pb2",
            devices: || vec![partial("fixture-pb2-v1")],
            options: OpenOptions::default,
            exercise: |v| {
                v.read_dir("pb2")?;
                v.read_dir("pb2/r447")?;
                read_files(v, &[".sbc.sf", "pb2/r447/big"])
            },
        },
        Case {
            name: "journal",
            devices: || vec![partial("fixture-journal-v1")],
            options: replay,
            exercise: |v| exercise_disks(v, &["probe/m.vmdk", "probe/src.vmdk"], false),
        },
        Case {
            name: "basic",
            devices: || vec![whole("fixture-basic-esxi8-v1")],
            options: OpenOptions::default,
            exercise: |v| {
                v.read_dir("files/many")?;
                read_disks(v, &["pat/lazy.vmdk"])
            },
        },
    ]
}

/// Values that sizes, counts and addresses do not survive.
fn interesting(rng: &mut Rng, old: u64, bits: u32) -> u64 {
    let max = if bits == 64 { u64::MAX } else { (1 << bits) - 1 };
    let value = match rng.below(12) {
        0 => 0,
        1 => 1,
        2 => max,
        3 => max - 1,
        4 => max >> 1,
        5 => (max >> 1) + 1,
        6 => old.wrapping_add(1),
        7 => old.wrapping_sub(1),
        8 => old << (1 + rng.below(u64::from(bits) - 1)),
        9 => old >> (1 + rng.below(u64::from(bits) - 1)),
        10 => old ^ (1 << rng.below(u64::from(bits))),
        _ => rng.next_u64(),
    };
    value & max
}

/// One damaged place: a byte, or a little-endian field of two, four or eight bytes at an offset that such a field
/// would have.
fn damage_at(rng: &mut Rng, device: &dyn BlockSource, at: u64) -> Vec<(u64, u8)> {
    let width = [1u64, 1, 2, 4, 4, 8, 8][rng.below(7) as usize];
    // Fields in VMFS structures are often unaligned (the LVM and FS descriptors are packed), so both happen.
    let at = if rng.below(2) == 0 { at - at % width } else { at };
    if at + width > device.len() {
        return Vec::new();
    }
    let mut old = [0u8; 8];
    device.read_exact_at(at, &mut old[..width as usize]).unwrap();
    let new = interesting(rng, u64::from_le_bytes(old), width as u32 * 8).to_le_bytes();
    (0..width as usize).map(|i| (at + i as u64, new[i])).collect()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut take = |flag: &str| -> Option<u64> {
        let i = args.iter().position(|a| a == flag)?;
        let value = args.remove(i + 1).parse().expect("a number");
        args.remove(i);
        Some(value)
    };
    let seed = take("--seed").unwrap_or(1);
    let rounds = take("--rounds").unwrap_or(20_000);
    let cases = cases();
    if args.iter().any(|a| a == "--list") {
        for case in &cases {
            println!("{}", case.name);
        }
        return;
    }
    for name in &args {
        assert!(cases.iter().any(|c| c.name == name), "no case named {name}");
    }

    // The watchdog: a run that does not end is a failure too, and nothing else would report it.
    let current: Arc<Mutex<Option<(Instant, String)>>> = Arc::new(Mutex::new(None));
    {
        let current = current.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            if let Some((started, what)) = current.lock().unwrap().as_ref() {
                if started.elapsed() > HANG {
                    println!("HANG {what}");
                    std::process::exit(1);
                }
            }
        });
    }
    // Panics are reported below, with what caused them.
    std::panic::set_hook(Box::new(|_| {}));

    let mut failures = 0u32;
    for case in cases
        .iter()
        .filter(|c| args.is_empty() || args.iter().any(|a| a == c.name))
    {
        let devices = (case.devices)();
        let open = |devices: Vec<Arc<dyn BlockSource>>| Volume::open_devices(devices, &(case.options)());
        let traces: Vec<Arc<TracingSource>> = devices
            .iter()
            .map(|device| Arc::new(TracingSource::new(device.clone())))
            .collect();
        let clean = Instant::now();
        let volume = open(traces.iter().map(|t| t.clone() as Arc<dyn BlockSource>).collect()).unwrap();
        (case.exercise)(&volume).unwrap();
        let clean = clean.elapsed();
        // (device, offset, length) of everything that was read, without large data reads.
        let ranges: Vec<(usize, u64, u64)> = traces
            .iter()
            .enumerate()
            .flat_map(|(device, trace)| trace.ranges().into_iter().map(move |(o, n)| (device, o, n)))
            .filter(|(_, _, len)| *len <= 4 * MIB)
            .collect();

        let mut rng = Rng::new(seed ^ case.name.bytes().fold(0u64, |h, b| h.wrapping_mul(131) ^ u64::from(b)));
        let (mut refused, mut slowest) = (0u32, Duration::ZERO);
        let started = Instant::now();
        for round in 0..rounds {
            let mut patches: Vec<Vec<(u64, u8)>> = vec![Vec::new(); devices.len()];
            for _ in 0..1 + rng.below(3) {
                let (device, start, len) = ranges[rng.below(ranges.len() as u64) as usize];
                // Most of what is read is zero padding. Prefer a place that holds something.
                let mut at = start + rng.below(len);
                let mut window = vec![0u8; ((start + len - at).min(4096)) as usize];
                devices[device].read_exact_at(at, &mut window).unwrap();
                let used: Vec<usize> = (0..window.len()).filter(|&i| window[i] != 0).collect();
                if !used.is_empty() && rng.below(8) != 0 {
                    at += used[rng.below(used.len() as u64) as usize] as u64;
                }
                patches[device].extend(damage_at(&mut rng, devices[device].as_ref(), at));
            }
            let what = format!("case {} seed {seed} round {round} patches {patches:?}", case.name);
            *current.lock().unwrap() = Some((Instant::now(), what.clone()));
            let damaged: Vec<Arc<dyn BlockSource>> = devices
                .iter()
                .zip(&patches)
                .map(|(device, patches)| {
                    Arc::new(MutatedSource::new(device.clone(), patches.clone())) as Arc<dyn BlockSource>
                })
                .collect();
            let run = Instant::now();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                open(damaged).and_then(|volume| (case.exercise)(&volume))
            }));
            let took = run.elapsed();
            *current.lock().unwrap() = None;
            slowest = slowest.max(took);
            match outcome {
                Ok(Ok(())) => {}
                // A read outside a device must be caught before it reaches the device.
                Ok(Err(error)) if error.kind() == ErrorKind::Io => {
                    println!("IO {what}: {error}");
                    failures += 1;
                }
                Ok(Err(_)) => refused += 1,
                Err(panic) => {
                    let message = panic
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                        .unwrap_or_default();
                    println!("PANIC {what}: {message}");
                    failures += 1;
                }
            }
            if took > SLOW {
                println!("SLOW ({took:.1?}, clean {clean:.1?}) {what}");
                failures += 1;
            }
        }
        println!(
            "{:8} {rounds} rounds in {:.0?}: {refused} refused, slowest {slowest:.1?} (clean {clean:.1?})",
            case.name,
            started.elapsed()
        );
    }
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|l| l.starts_with("VmHWM")) {
            println!("peak memory: {}", line.trim_start_matches("VmHWM:").trim());
        }
    }
    std::process::exit(i32::from(failures > 0));
}
