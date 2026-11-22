//! SESparse snapshot deltas: the "space-efficient sparse" format ESXi uses for snapshots on VMFS6.
//!
//! A delta holds the 4 KiB blocks ("grains") written since the snapshot was taken. Everything it does not hold comes
//! from its parent.
//!
//! Format description: `docs/format/80-sesparse.md`.

use std::sync::Arc;

use crate::bytes::{add, mul, View};
use crate::error::{Error, Result};
use crate::io::{check_range, read_vec, BlockSource};

const SECTOR: u64 = 512;
const HEADER_SIZE: u64 = 512;
const CONST_MAGIC: u64 = 0x0000_0000_CAFE_BABE;
const VOLATILE_MAGIC: u64 = 0x0000_0000_CAFE_CAFE;
const VERSION: u64 = 0x0000_0002_0000_0001;
/// Sectors per grain: 4 KiB.
const GRAIN_SECTORS: u64 = 8;
const GRAIN_SIZE: u64 = GRAIN_SECTORS * SECTOR;
/// Sectors per grain table: 4096 entries of 8 bytes.
const TABLE_SECTORS: u64 = 64;
const ENTRIES_PER_TABLE: u64 = TABLE_SECTORS * SECTOR / 8;
/// Upper half of a grain directory entry that points to a table.
const DIRECTORY_ENTRY_TAG: u64 = 0x1000_0000;
/// How many grain tables are fetched in one read at most (4 MiB).
const TABLES_PER_READ: u64 = 128;

/// What a delta says about a range of the disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunKind {
    /// The delta holds the data, at this offset of the delta file.
    Allocated { file_offset: u64 },
    /// The delta says the range is zero, whatever the parent holds.
    Zero,
}

/// A range of the disk that the delta defines. Ranges without a run come from the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Run {
    pub offset: u64,
    pub len: u64,
    pub kind: RunKind,
}

/// An opened delta. Its tables are resolved once; afterwards it is immutable.
#[derive(Debug)]
pub(crate) struct SeSparse {
    source: Arc<dyn BlockSource>,
    capacity: u64,
    /// Sorted by offset, not overlapping.
    runs: Box<[Run]>,
}

impl SeSparse {
    /// Opens the delta stored in `source`. `capacity` is the disk size the descriptor announces.
    pub(crate) fn open(source: Arc<dyn BlockSource>, capacity: u64, accept_unverified: bool) -> Result<Self> {
        let raw = read_vec(&*source, 0, HEADER_SIZE, "SESparse header")?;
        let header = View::new("SESparse header", 0, &raw);

        let magic = header.u64(0)?;
        if magic != CONST_MAGIC {
            return Err(Error::corrupt(format!(
                "SESparse header: magic {magic:#018x}, expected {CONST_MAGIC:#018x}"
            )));
        }
        // Every field is compared with the value found in deltas written by ESXi 7.0.3 (docs/format/80-sesparse.md).
        let expect = |name: &str, offset: usize, expected: u64| -> Result<()> {
            let value = header.u64(offset)?;
            if value == expected {
                Ok(())
            } else {
                Err(Error::unsupported(format!(
                    "SESparse header: {name} is {value:#x}, only {expected:#x} has been verified"
                )))
            }
        };
        expect("version", 8, VERSION)?;
        expect("grain size", 24, GRAIN_SECTORS)?;
        expect("grain table size", 32, TABLE_SECTORS)?;
        expect("flags", 40, 0)?;
        // The first of four fields without a known meaning is 0 in deltas written by ESXi 7.0.3 and 0x200 in deltas
        // written by ESXi 8.0 U3. Everything else in the format is the same; both read identical to ESXi's own
        // consolidation.
        let first_reserved = header.u64(48)?;
        if first_reserved != 0 && first_reserved != 0x200 {
            return Err(Error::unsupported(format!(
                "SESparse header: reserved field 0 is {first_reserved:#x}, only 0x0 and 0x200 have been verified"
            )));
        }
        for (index, offset) in [56usize, 64, 72].into_iter().enumerate() {
            expect(&format!("reserved field {}", index.saturating_add(1)), offset, 0)?;
        }
        if header.bytes(208, 304)?.iter().any(|&b| b != 0) {
            return Err(Error::unsupported("SESparse header: padding is not zero"));
        }
        let capacity_sectors = header.u64(16)?;
        if mul(capacity_sectors, SECTOR, "SESparse capacity")? != capacity {
            return Err(Error::corrupt(format!(
                "SESparse header: capacity {capacity_sectors} sectors, but the descriptor says {} bytes",
                capacity
            )));
        }

        let volatile_offset = mul(header.u64(80)?, SECTOR, "SESparse volatile header offset")?;
        let raw_volatile = read_vec(&*source, volatile_offset, HEADER_SIZE, "SESparse volatile header")?;
        let volatile = View::new("SESparse volatile header", volatile_offset, &raw_volatile);
        let volatile_magic = volatile.u64(0)?;
        if volatile_magic != VOLATILE_MAGIC {
            return Err(Error::corrupt(format!(
                "SESparse volatile header: magic {volatile_magic:#018x}, expected {VOLATILE_MAGIC:#018x}"
            )));
        }
        if volatile.u64(24)? != 0 {
            return Err(Error::unsupported(
                "the SESparse delta was not closed cleanly (its journal would have to be replayed); \
                 this is the state of a delta that a running VM is writing to",
            ));
        }
        if volatile.bytes(32, 480)?.iter().any(|&b| b != 0) {
            return Err(Error::unsupported("SESparse volatile header: padding is not zero"));
        }

        let directory_offset = mul(header.u64(128)?, SECTOR, "SESparse grain directory offset")?;
        let directory_size = mul(header.u64(136)?, SECTOR, "SESparse grain directory size")?;
        let tables_offset = mul(header.u64(144)?, SECTOR, "SESparse grain tables offset")?;
        let tables_size = mul(header.u64(152)?, SECTOR, "SESparse grain tables size")?;
        let grains_offset = mul(header.u64(192)?, SECTOR, "SESparse grains offset")?;
        check_range(
            "SESparse grain directory",
            source.len(),
            directory_offset,
            directory_size,
        )?;
        check_range("SESparse grain tables", source.len(), tables_offset, tables_size)?;

        let total_grains = capacity.div_ceil(GRAIN_SIZE);
        let tables_needed = total_grains.div_ceil(ENTRIES_PER_TABLE);
        let directory_bytes = mul(tables_needed, 8, "SESparse grain directory length")?;
        if directory_bytes > directory_size {
            return Err(Error::corrupt(format!(
                "SESparse grain directory has room for {} tables, the disk needs {tables_needed}",
                directory_size / 8
            )));
        }
        let raw_directory = read_vec(&*source, directory_offset, directory_bytes, "SESparse grain directory")?;
        let table_bytes = TABLE_SECTORS * SECTOR;

        let directory: Vec<u64> = raw_directory
            .chunks_exact(8)
            .map(|raw_entry| u64::from_le_bytes(raw_entry.try_into().unwrap_or([0; 8])))
            .collect();
        drop(raw_directory);
        // Which tables the disk uses, as `(index in the table area, number in the directory)`.
        let mut wanted: Vec<(u64, u64)> = Vec::new();
        for (table_number, &entry) in directory.iter().enumerate() {
            if entry == 0 {
                continue;
            }
            if entry >> 32 != DIRECTORY_ENTRY_TAG {
                return Err(Error::unsupported(format!(
                    "SESparse grain directory entry {table_number} is {entry:#018x}; only entries of the form \
                     0x10000000_<table index> have been verified"
                )));
            }
            let table_index = entry & 0xFFFF_FFFF;
            let table_end = mul(
                add(table_index, 1, "grain table index")?,
                table_bytes,
                "grain table end",
            )?;
            if table_end > tables_size {
                return Err(Error::corrupt(format!(
                    "SESparse grain directory entry {table_number} points to table {table_index}, outside the table area"
                )));
            }
            wanted.push((table_index, table_number as u64));
        }
        drop(directory);
        // ESXi hands out tables in the order a guest first touches their part of the disk, so the directory names
        // them in any order. Each read costs a round trip to the device: walk the tables in the order of the file
        // and fetch neighbours together.
        wanted.sort_unstable();

        let fit = |value: u64| {
            usize::try_from(value).map_err(|_| Error::corrupt("SESparse grain table position does not fit"))
        };
        let table_len = fit(table_bytes)?;
        let mut runs: Vec<Run> = Vec::new();
        let mut rest = wanted.as_slice();
        while let Some(&(first_index, _)) = rest.first() {
            let mut tables = 1usize;
            while (tables as u64) < TABLES_PER_READ
                && rest.get(tables).map(|next| next.0) == first_index.checked_add(tables as u64)
            {
                tables = tables.saturating_add(1);
            }
            let (batch, later) = rest.split_at(tables);
            rest = later;
            let raw = read_vec(
                &*source,
                add(
                    tables_offset,
                    mul(first_index, table_bytes, "grain table offset")?,
                    "grain table offset",
                )?,
                mul(tables as u64, table_bytes, "grain table length")?,
                "SESparse grain table",
            )?;
            for (raw_table, &(table_index, table_number)) in raw.chunks_exact(table_len).zip(batch) {
                // Runs grow only inside one table here; tables are joined below, once they are in disk order.
                let first_run = runs.len();
                let first_grain = mul(table_number, ENTRIES_PER_TABLE, "grain number")?;
                for (slot, raw_grain) in raw_table.chunks_exact(8).enumerate() {
                    let grain_entry = u64::from_le_bytes(raw_grain.try_into().unwrap_or([0; 8]));
                    if grain_entry == 0 {
                        continue;
                    }
                    let grain = add(first_grain, slot as u64, "grain number")?;
                    if grain >= total_grains {
                        return Err(Error::corrupt(format!(
                            "SESparse grain table {table_index} has an entry for grain {grain}, beyond the disk"
                        )));
                    }
                    let kind = decode_grain(grain_entry, grains_offset, source.len(), accept_unverified)?;
                    push_run(
                        &mut runs,
                        first_run,
                        mul(grain, GRAIN_SIZE, "grain offset")?,
                        capacity,
                        kind,
                    )?;
                }
            }
        }
        drop(wanted);
        runs.sort_unstable_by_key(|run| run.offset);
        runs.dedup_by(|next, last| {
            let joins = last.offset.checked_add(last.len) == Some(next.offset) && continues(last, next.kind);
            if joins {
                last.len = last.len.saturating_add(next.len);
            }
            joins
        });
        runs.shrink_to_fit();
        Ok(SeSparse {
            source,
            capacity,
            runs: runs.into_boxed_slice(),
        })
    }

    pub(crate) fn runs(&self) -> &[Run] {
        &self.runs
    }

    /// Fills `buf` with the disk content at `offset`. Ranges this delta does not define are read through `parent`.
    pub(crate) fn read_exact_at(
        &self,
        mut offset: u64,
        mut buf: &mut [u8],
        parent: &dyn Fn(u64, &mut [u8]) -> Result<()>,
    ) -> Result<()> {
        check_range("virtual disk", self.capacity, offset, buf.len() as u64)?;
        // First run that ends after `offset`.
        let mut index = self
            .runs
            .partition_point(|run| run.offset.saturating_add(run.len) <= offset);
        while !buf.is_empty() {
            let run = self.runs.get(index);
            // Bytes up to the next run come from the parent.
            let gap = match run {
                Some(run) if run.offset <= offset => 0,
                Some(run) => run.offset.saturating_sub(offset),
                None => u64::MAX,
            };
            if gap > 0 {
                let take = usize::try_from(gap).unwrap_or(usize::MAX).min(buf.len());
                let (head, tail) = buf.split_at_mut(take);
                parent(offset, head)?;
                buf = tail;
                offset = add(offset, take as u64, "disk offset")?;
                continue;
            }
            let Some(run) = run else { break };
            let within = offset.saturating_sub(run.offset);
            let take = usize::try_from(run.len.saturating_sub(within))
                .unwrap_or(usize::MAX)
                .min(buf.len());
            let (head, tail) = buf.split_at_mut(take);
            match run.kind {
                RunKind::Allocated { file_offset } => {
                    self.source
                        .read_exact_at(add(file_offset, within, "delta file offset")?, head)?;
                }
                RunKind::Zero => head.fill(0),
            }
            buf = tail;
            offset = add(offset, take as u64, "disk offset")?;
            index = index.saturating_add(1);
        }
        Ok(())
    }
}

/// Decodes a non-zero grain table entry. The type is in the top four bits.
fn decode_grain(entry: u64, grains_offset: u64, file_len: u64, accept_unverified: bool) -> Result<RunKind> {
    match entry >> 60 {
        // Allocated: the grain index is split, its low 12 bits are stored in bits 48..=59.
        3 => {
            let index = ((entry >> 48) & 0xFFF) | ((entry & 0xFFFF_FFFF_FFFF) << 12);
            let file_offset = add(
                grains_offset,
                mul(index, GRAIN_SIZE, "grain position")?,
                "grain position",
            )?;
            check_range("SESparse grain", file_len, file_offset, GRAIN_SIZE)
                .map_err(|e| e.context(format!("grain table entry {entry:#018x}")))?;
            Ok(RunKind::Allocated { file_offset })
        }
        // The guest discarded the range (SCSI unmap). Verified on deltas written by ESXi 7.0.3: the range reads as
        // zeros even where the parent holds data. The remaining bits may still name the grain that used to hold
        // the data; they are not used.
        1 => Ok(RunKind::Zero),
        // Zero grain. Other readers treat it as zeros; it has not been seen in a delta written by ESXi yet.
        2 => {
            if accept_unverified {
                Ok(RunKind::Zero)
            } else {
                Err(Error::unsupported(format!(
                    "SESparse grain table entry {entry:#018x} is a zero grain (type 2); how to read it has not been \
                     verified against data written by ESXi yet"
                )))
            }
        }
        0 => Err(Error::corrupt(format!(
            "SESparse grain table entry {entry:#018x} has type 0 (unallocated) but is not zero"
        ))),
        other => Err(Error::corrupt(format!(
            "SESparse grain table entry {entry:#018x} has unknown type {other}"
        ))),
    }
}

/// Whether a range of `kind` that starts where `last` ends is stored right behind it.
fn continues(last: &Run, kind: RunKind) -> bool {
    match (last.kind, kind) {
        (RunKind::Zero, RunKind::Zero) => true,
        (RunKind::Allocated { file_offset: a }, RunKind::Allocated { file_offset: b }) => {
            a.checked_add(last.len) == Some(b)
        }
        _ => false,
    }
}

/// Adds one grain. It extends the last run if that run was added at or after position `from` and continues here.
fn push_run(runs: &mut Vec<Run>, from: usize, offset: u64, capacity: u64, kind: RunKind) -> Result<()> {
    // The last grain of a disk whose size is not a multiple of the grain size is shorter.
    let len = GRAIN_SIZE.min(capacity.saturating_sub(offset));
    if runs.len() > from {
        if let Some(last) = runs.last_mut() {
            if add(last.offset, last.len, "run end")? == offset && continues(last, kind) {
                last.len = add(last.len, len, "run length")?;
                return Ok(());
            }
        }
    }
    runs.push(Run { offset, len, kind });
    Ok(())
}

/// Number of bytes a run list would need in memory per entry; exposed for documentation of the cost model.
#[allow(dead_code)]
pub(crate) const RUN_SIZE: usize = std::mem::size_of::<Run>();

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::io::MemorySource;

    const MIB: u64 = 1 << 20;

    /// Layout of the test images, in sectors: volatile header 1, directory 16, tables 32 (4 tables), grains 512.
    const DIR: usize = 16;
    const TABLES: usize = 32;
    const GRAINS: usize = 512;

    fn put(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    /// Entry for an allocated grain with the given index.
    pub(crate) fn allocated(index: u64) -> u64 {
        (3 << 60) | ((index & 0xFFF) << 48) | (index >> 12)
    }

    /// Builds a delta for a disk of `capacity` bytes. `grains` maps disk grain numbers to table entries; grain
    /// `index` of the grain area is filled with the byte `index as u8 + 1`.
    pub(crate) fn build(capacity: u64, grains: &[(u64, u64)], grain_area: u64) -> Vec<u8> {
        let mut image = vec![0u8; GRAINS * 512 + (grain_area * 4096) as usize];
        put(&mut image, 0, CONST_MAGIC);
        put(&mut image, 8, VERSION);
        put(&mut image, 16, capacity / 512);
        put(&mut image, 24, 8);
        put(&mut image, 32, 64);
        put(&mut image, 80, 1);
        put(&mut image, 88, 1);
        put(&mut image, 128, DIR as u64);
        put(&mut image, 136, 16);
        put(&mut image, 144, TABLES as u64);
        put(&mut image, 152, 4 * 64);
        put(&mut image, 192, GRAINS as u64);
        put(&mut image, 200, capacity / 512);
        put(&mut image, 512, VOLATILE_MAGIC);
        let mut tables: Vec<u64> = Vec::new();
        for &(grain, entry) in grains {
            let table_number = grain / 4096;
            let table_index = match tables.iter().position(|&t| t == table_number) {
                Some(index) => index,
                None => {
                    tables.push(table_number);
                    put(
                        &mut image,
                        DIR * 512 + table_number as usize * 8,
                        (0x1000_0000 << 32) | (tables.len() as u64 - 1),
                    );
                    tables.len() - 1
                }
            };
            put(
                &mut image,
                (TABLES + table_index * 64) * 512 + (grain % 4096) as usize * 8,
                entry,
            );
        }
        for index in 0..grain_area {
            let at = GRAINS * 512 + (index * 4096) as usize;
            image[at..at + 4096].fill(index as u8 + 1);
        }
        image
    }

    fn open(image: Vec<u8>, capacity: u64) -> Result<SeSparse> {
        SeSparse::open(Arc::new(MemorySource::new(image)), capacity, false)
    }

    fn parent_ff(_: u64, buf: &mut [u8]) -> Result<()> {
        buf.fill(0xFF);
        Ok(())
    }

    #[test]
    fn allocated_grains_come_from_the_delta_and_the_rest_from_the_parent() {
        // Grains 2 and 3 are adjacent on the disk and in the file: one run. Grain 5000 is in the second table.
        let image = build(
            32 * MIB,
            &[(2, allocated(0)), (3, allocated(1)), (5000, allocated(2))],
            3,
        );
        let delta = open(image, 32 * MIB).unwrap();
        assert_eq!(
            delta.runs(),
            &[
                Run {
                    offset: 2 * 4096,
                    len: 2 * 4096,
                    kind: RunKind::Allocated { file_offset: 512 * 512 }
                },
                Run {
                    offset: 5000 * 4096,
                    len: 4096,
                    kind: RunKind::Allocated {
                        file_offset: 512 * 512 + 2 * 4096
                    }
                },
            ]
        );
        // A read that starts in the parent, crosses both grains and ends in the parent again.
        let mut buf = vec![0u8; 4 * 4096];
        delta.read_exact_at(4096, &mut buf, &parent_ff).unwrap();
        assert!(buf[..4096].iter().all(|&b| b == 0xFF));
        assert!(buf[4096..2 * 4096].iter().all(|&b| b == 1));
        assert!(buf[2 * 4096..3 * 4096].iter().all(|&b| b == 2));
        assert!(buf[3 * 4096..].iter().all(|&b| b == 0xFF));
        // A read inside one grain, not aligned.
        let mut small = [0u8; 10];
        delta.read_exact_at(5000 * 4096 + 100, &mut small, &parent_ff).unwrap();
        assert_eq!(small, [3u8; 10]);
        // Beyond the last run, and beyond the disk.
        delta.read_exact_at(32 * MIB - 10, &mut small, &parent_ff).unwrap();
        assert_eq!(small, [0xFFu8; 10]);
        assert!(delta.read_exact_at(32 * MIB - 5, &mut small, &parent_ff).is_err());
    }

    #[test]
    fn the_grain_index_is_split_across_the_entry() {
        // Index 0x1234: low 12 bits 0x234 in bits 48..=59, the rest (1) in the low bits. Seen on ESXi:
        // 0x3001000000000000 is index 1.
        assert_eq!(allocated(1), 0x3001_0000_0000_0000);
        assert_eq!(allocated(0x1234), 0x3234_0000_0000_0001);
        let kind = decode_grain(allocated(0x1234), 6 * MIB, u64::MAX, false).unwrap();
        assert_eq!(
            kind,
            RunKind::Allocated {
                file_offset: 6 * MIB + 0x1234 * 4096
            }
        );
    }

    #[test]
    fn an_empty_delta_defines_nothing() {
        let delta = open(build(32 * MIB, &[], 0), 32 * MIB).unwrap();
        assert!(delta.runs().is_empty());
        let mut buf = [0u8; 16];
        delta.read_exact_at(MIB, &mut buf, &parent_ff).unwrap();
        assert_eq!(buf, [0xFF; 16]);
    }

    #[test]
    fn a_range_discarded_by_the_guest_reads_as_zeros_not_as_the_parent() {
        // Entries as ESXi wrote them: plain, and with the bits of the grain that used to hold the data.
        let image = build(32 * MIB, &[(7, 0x1000_0000_0000_0000), (8, 0x1100_0000_0000_0000)], 0);
        let delta = open(image, 32 * MIB).unwrap();
        assert_eq!(
            delta.runs(),
            &[Run {
                offset: 7 * 4096,
                len: 2 * 4096,
                kind: RunKind::Zero
            }]
        );
        let mut buf = [0xAAu8; 3 * 4096];
        delta.read_exact_at(6 * 4096, &mut buf, &parent_ff).unwrap();
        assert!(
            buf[..4096].iter().all(|&b| b == 0xFF),
            "before the discarded range: parent"
        );
        assert!(
            buf[4096..].iter().all(|&b| b == 0),
            "discarded range: zeros, although the parent has data"
        );
    }

    #[test]
    fn zero_grains_are_refused_until_seen_on_esxi() {
        let zero = build(32 * MIB, &[(7, 2 << 60)], 0);
        assert_eq!(open(zero.clone(), 32 * MIB).unwrap_err().kind(), ErrorKind::Unsupported);
        let delta = SeSparse::open(Arc::new(MemorySource::new(zero)), 32 * MIB, true).unwrap();
        assert_eq!(
            delta.runs(),
            &[Run {
                offset: 7 * 4096,
                len: 4096,
                kind: RunKind::Zero
            }]
        );
    }

    #[test]
    fn a_delta_that_was_not_closed_cleanly_is_refused() {
        let mut image = build(32 * MIB, &[(2, allocated(0))], 1);
        put(&mut image, 512 + 24, 1);
        let error = open(image, 32 * MIB).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Unsupported);
        assert!(error.to_string().contains("not closed cleanly"));
    }

    #[test]
    fn damaged_or_unknown_deltas_are_refused() {
        let good = build(32 * MIB, &[(2, allocated(0))], 1);
        let kind = |patch: &dyn Fn(&mut Vec<u8>)| {
            let mut image = good.clone();
            patch(&mut image);
            open(image, 32 * MIB).unwrap_err().kind()
        };
        assert_eq!(kind(&|i| i[0] = 0), ErrorKind::Corrupt); // magic
        assert_eq!(kind(&|i| i[8] = 2), ErrorKind::Unsupported); // version
        assert_eq!(kind(&|i| i[24] = 16), ErrorKind::Unsupported); // grain size
        assert_eq!(kind(&|i| i[32] = 32), ErrorKind::Unsupported); // table size
        assert_eq!(kind(&|i| i[40] = 1), ErrorKind::Unsupported); // flags
        assert_eq!(kind(&|i| i[300] = 1), ErrorKind::Unsupported); // padding
        assert_eq!(kind(&|i| i[512] = 0), ErrorKind::Corrupt); // volatile magic
        assert_eq!(kind(&|i| i[16] = 1), ErrorKind::Corrupt); // capacity differs from the descriptor
                                                              // A directory entry of an unknown form, and one that points outside the table area.
        assert_eq!(
            kind(&|i| put(i, DIR * 512, 0x2000_0000_0000_0000)),
            ErrorKind::Unsupported
        );
        assert_eq!(
            kind(&|i| put(i, DIR * 512, (0x1000_0000 << 32) | 99)),
            ErrorKind::Corrupt
        );
        // A grain outside the file, a non-zero "unallocated" entry and an unknown type.
        let table0 = TABLES * 512;
        assert_eq!(kind(&|i| put(i, table0 + 16, allocated(1_000_000))), ErrorKind::Corrupt);
        assert_eq!(kind(&|i| put(i, table0 + 16, 5)), ErrorKind::Corrupt);
        assert_eq!(kind(&|i| put(i, table0 + 16, 7 << 60)), ErrorKind::Corrupt);
        assert_eq!(
            open(good[..600].to_vec(), 32 * MIB).unwrap_err().kind(),
            ErrorKind::Corrupt
        );
    }

    #[test]
    fn an_entry_beyond_the_disk_is_corrupt() {
        // 16 MiB disk: 4096 grains, exactly one table. Grain 4096 would be in a second table.
        let image = build(16 * MIB + 4096, &[(4097, allocated(0))], 1);
        assert_eq!(open(image, 16 * MIB + 4096).unwrap_err().kind(), ErrorKind::Corrupt);
    }

    /// Records the length of every read.
    #[derive(Debug)]
    struct Recording(MemorySource, std::sync::Mutex<Vec<usize>>);

    impl BlockSource for Recording {
        fn len(&self) -> u64 {
            self.0.len()
        }

        fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
            self.1.lock().unwrap().push(buf.len());
            self.0.read_exact_at(offset, buf)
        }
    }

    #[test]
    fn neighbouring_tables_are_read_together() {
        // Tables are numbered in the order they are first used: disk tables 2, 0, 1, 3 get the indices 0, 1, 2, 3.
        // The directory names them as 1, 2, 0, 3; in the file they lie next to each other, so one read fetches all.
        let grains = [
            (2 * 4096, allocated(0)),
            (5, allocated(1)),
            (4096 + 7, allocated(2)),
            (3 * 4096 + 9, allocated(3)),
        ];
        let source = Arc::new(Recording(
            MemorySource::new(build(64 * MIB, &grains, 4)),
            Default::default(),
        ));
        let delta = SeSparse::open(source.clone(), 64 * MIB, false).unwrap();
        let table = (TABLE_SECTORS * SECTOR) as usize;
        let table_reads: Vec<usize> = source
            .1
            .lock()
            .unwrap()
            .iter()
            .copied()
            .filter(|&len| len >= table)
            .collect();
        assert_eq!(table_reads, [4 * table]);
        for (grain, index) in [(2 * 4096u64, 0u8), (5, 1), (4096 + 7, 2), (3 * 4096 + 9, 3)] {
            let mut buf = [0u8; 4096];
            delta
                .read_exact_at(grain * 4096, &mut buf, &|_, _| panic!("parent"))
                .unwrap();
            assert!(buf.iter().all(|&b| b == index + 1), "grain {grain}");
        }
    }

    #[test]
    fn runs_join_across_tables_whatever_order_the_tables_have() {
        // The table of the second 16 MiB was used first. Its first grain lies behind the last grain of the first
        // 16 MiB, on the disk and in the delta, so both form one run.
        let grains = [(4096, allocated(1)), (4095, allocated(0)), (7, allocated(5))];
        let delta = open(build(64 * MIB, &grains, 8), 64 * MIB).unwrap();
        let runs: Vec<(u64, u64)> = delta.runs().iter().map(|run| (run.offset, run.len)).collect();
        assert_eq!(runs, [(7 * 4096, 4096), (4095 * 4096, 2 * 4096)]);
    }
}
