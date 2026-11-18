//! The VMFS6 metadata journal: finding transactions that were written to a journal but not yet to their final place,
//! and reading the volume as if they had been.
//!
//! Format description: `docs/format/60-heartbeat-journal.md`. Everything here was worked out from images of one
//! datastore made by ESXi 7.0.3; anything that does not look exactly like those images is refused.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::bytes::{add, mul, to_usize, View};
use crate::error::{Error, Result, ResultExt};
use crate::io::{check_range, read_vec, BlockSource};

/// Volume offset of the heartbeat region and its layout.
const HEARTBEAT_OFFSET: u64 = 0x30_0000;
const HEARTBEAT_SLOTS: u64 = 1024;
const HEARTBEAT_SLOT_SIZE: u64 = 4096;
/// Offset of the owner's journal (a volume offset) inside a heartbeat slot.
const HEARTBEAT_JOURNAL_OFFSET: usize = 0x64;

/// Where the filesystem descriptor records the size of a journal.
const JOURNAL_SIZE_FIELD: u64 = 0x20_0000 + 0x148;
/// The only journal size seen: 2 MiB, one header block and 511 transaction slots.
const JOURNAL_SIZE: u64 = 2 << 20;
const SLOT_SIZE: u64 = 4096;
const SLOT_SIZE_BYTES: usize = 4096;

/// "6JSF" at the start of a journal, "HJ6J" 32 bytes further.
const JOURNAL_MAGIC: u32 = 0x4653_4A36;
const JOURNAL_MAGIC_2: u32 = 0x4A36_4A48;
/// "HT6J": a transaction that has been written to the journal but not yet to its final place.
const SLOT_PENDING: u32 = 0x4A36_5448;
/// "TO6J": a slot whose transaction has reached its final place, or that was never used.
const SLOT_DONE: u32 = 0x4A36_4F54;
/// Set in the flags of every pending transaction seen.
const FLAG_PENDING: u16 = 0x2;

/// Entry types of the two lists in a transaction: blocks it locks, and blocks it changes.
const ENTRY_LOCK: [u32; 2] = [0x200, 0x1_0200];
const ENTRY_UPDATE: [u32; 2] = [0x105, 0x1_0105];
const UPDATE_HEADER_SIZE: usize = 0x28;
/// Most entries one list may have; a slot of 4096 bytes cannot hold more.
const MAX_ENTRIES: usize = 128;

/// A change to a range of bytes of one metadata block.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Patch {
    /// Volume offset of the first byte.
    offset: u64,
    data: Vec<u8>,
}

/// A transaction that waits in a journal.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Transaction {
    journal: u64,
    sequence: u64,
    patches: Vec<Patch>,
}

/// What the journals of a volume hold.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pending {
    /// Journals that heartbeat slots point to.
    pub journals: u32,
    transactions: Vec<Transaction>,
}

impl Pending {
    /// Number of transactions that were written to a journal but not yet to their final place.
    pub fn transactions(&self) -> u32 {
        u32::try_from(self.transactions.len()).unwrap_or(u32::MAX)
    }
}

/// Reads the journals that heartbeat slots point to and collects their pending transactions.
pub(crate) fn scan(volume: &dyn BlockSource) -> Result<Pending> {
    let region_size = mul(HEARTBEAT_SLOTS, HEARTBEAT_SLOT_SIZE, "heartbeat region")?;
    let region = read_vec(volume, HEARTBEAT_OFFSET, region_size, "heartbeat region")?;
    let mut journals: Vec<u64> = Vec::new();
    for slot in region.chunks_exact(SLOT_SIZE_BYTES) {
        let offset = View::new("heartbeat slot", HEARTBEAT_OFFSET, slot).u64(HEARTBEAT_JOURNAL_OFFSET)?;
        if offset != 0 && !journals.contains(&offset) {
            journals.push(offset);
        }
    }
    let mut pending = Pending {
        journals: u32::try_from(journals.len()).unwrap_or(u32::MAX),
        transactions: Vec::new(),
    };
    if journals.is_empty() {
        return Ok(pending);
    }

    let raw_size = read_vec(
        volume,
        JOURNAL_SIZE_FIELD,
        4,
        "journal size in the filesystem descriptor",
    )?;
    let journal_size = u64::from(View::new("filesystem descriptor", JOURNAL_SIZE_FIELD, &raw_size).u32(0)?);
    if journal_size != JOURNAL_SIZE {
        return Err(Error::unsupported(format!(
            "journals of {journal_size:#x} bytes (only {JOURNAL_SIZE:#x} has been seen)"
        )));
    }
    for journal in journals {
        read_journal(volume, journal, &mut pending.transactions)
            .context(format!("journal at volume offset {journal:#x}"))?;
    }
    // Two hosts changing the same block would need an order between their journals, which is not known.
    let mut owner: BTreeMap<u64, u64> = BTreeMap::new();
    for transaction in &pending.transactions {
        for patch in &transaction.patches {
            let block = patch.offset & !(SLOT_SIZE - 1);
            if *owner.entry(block).or_insert(transaction.journal) != transaction.journal {
                return Err(Error::unsupported(format!(
                    "two journals hold pending changes to the block at volume offset {block:#x}; their order is not known"
                )));
            }
        }
    }
    Ok(pending)
}

fn read_journal(volume: &dyn BlockSource, journal: u64, out: &mut Vec<Transaction>) -> Result<()> {
    if journal % SLOT_SIZE != 0 {
        return Err(Error::corrupt("the journal does not start on a block boundary"));
    }
    check_range("journal", volume.len(), journal, JOURNAL_SIZE)?;
    let raw = read_vec(volume, journal, JOURNAL_SIZE, "journal")?;
    let header = View::new("journal header", journal, &raw);
    let (magic, magic_2) = (header.u32(0x00)?, header.u32(0x20)?);
    if magic != JOURNAL_MAGIC || magic_2 != JOURNAL_MAGIC_2 {
        return Err(Error::unsupported(format!(
            "journal signatures {magic:#010x} and {magic_2:#010x}, expected {JOURNAL_MAGIC:#010x} and {JOURNAL_MAGIC_2:#010x}"
        )));
    }
    let mut found = Vec::new();
    for (index, slot) in raw.chunks_exact(SLOT_SIZE_BYTES).enumerate().skip(1) {
        let slot_offset = add(journal, mul(index as u64, SLOT_SIZE, "journal slot")?, "journal slot")?;
        let view = View::new("journal slot", slot_offset, slot);
        match view.u32(0x00)? {
            SLOT_DONE => {}
            SLOT_PENDING => {
                let transaction = read_transaction(&view, slot, journal, volume.len()).context(format!(
                    "pending transaction in the slot at volume offset {slot_offset:#x}"
                ))?;
                found.push(transaction);
            }
            other => {
                return Err(Error::unsupported(format!(
                    "journal slot at volume offset {slot_offset:#x} has the signature {other:#010x}, which is not known"
                )));
            }
        }
    }
    found.sort_by_key(|transaction| transaction.sequence);
    if found
        .windows(2)
        .any(|pair| matches!(pair, [a, b] if a.sequence == b.sequence))
    {
        return Err(Error::corrupt("two pending transactions have the same sequence number"));
    }
    out.append(&mut found);
    Ok(())
}

fn read_transaction(view: &View<'_>, slot: &[u8], journal: u64, volume_size: u64) -> Result<Transaction> {
    let flags = view.u16(0x06)?;
    if flags & FLAG_PENDING == 0 {
        return Err(Error::unsupported(format!(
            "flags {flags:#06x} on a pending transaction"
        )));
    }
    let sequence = view.u64(0x18)?;
    if view.u64(0x40)? != sequence || view.u64(0x48)? != sequence {
        return Err(Error::unsupported("a transaction whose sequence fields differ"));
    }
    let length = to_usize(u64::from(view.u32(0x30)?), "transaction length")?;
    if length > SLOT_SIZE_BYTES {
        return Err(Error::unsupported(format!(
            "a transaction of {length} bytes, larger than a journal slot"
        )));
    }
    let (first_lock, first_update) = (view.u32(0x50)?, view.u32(0x54)?);
    let (lock_count, update_count) = (usize::from(view.u16(0x58)?), usize::from(view.u16(0x5A)?));

    // The blocks the transaction locks: only walked to check that the slot has the expected shape.
    let mut seen = 0usize;
    let mut at = to_usize(u64::from(first_lock), "lock entry")?;
    while at != 0 {
        if seen >= MAX_ENTRIES || view.u32(at)? as usize != at {
            return Err(Error::corrupt(format!("lock entry at {at:#x} is out of place")));
        }
        let kind = view.u32(at.saturating_add(8))?;
        if !ENTRY_LOCK.contains(&kind) {
            return Err(Error::unsupported(format!("lock entry of type {kind:#x}")));
        }
        seen = seen.saturating_add(1);
        at = to_usize(u64::from(view.u32(at.saturating_add(4))?), "lock entry")?;
    }
    if seen != lock_count {
        return Err(Error::corrupt(format!(
            "{seen} lock entries, the header says {lock_count}"
        )));
    }

    let mut patches = Vec::new();
    let mut seen = 0usize;
    let mut at = to_usize(u64::from(first_update), "update entry")?;
    while at != 0 {
        if seen >= MAX_ENTRIES || view.u32(at)? as usize != at {
            return Err(Error::corrupt(format!("update entry at {at:#x} is out of place")));
        }
        let next = to_usize(u64::from(view.u32(at.saturating_add(4))?), "update entry")?;
        let kind = view.u32(at.saturating_add(8))?;
        if !ENTRY_UPDATE.contains(&kind) {
            return Err(Error::unsupported(format!("update entry of type {kind:#x}")));
        }
        let target = view.u64(at.saturating_add(0x0C))?;
        let patch_bytes = to_usize(u64::from(view.u32(at.saturating_add(0x14))?), "update size")?;
        let block_size = u64::from(view.u32(at.saturating_add(0x18))?);
        if block_size != SLOT_SIZE || target % SLOT_SIZE != 0 {
            return Err(Error::unsupported(format!(
                "an update of a block of {block_size} bytes at volume offset {target:#x}"
            )));
        }
        if to_usize(u64::from(view.u32(at.saturating_add(0x1C))?), "update size")? != patch_bytes {
            return Err(Error::unsupported("an update entry whose two size fields differ"));
        }
        check_range("journal update target", volume_size, target, block_size)?;

        // The changes: (offset in the block, length, bytes), one after the other.
        let mut position = at.saturating_add(UPDATE_HEADER_SIZE);
        let end = position.saturating_add(patch_bytes);
        if end > length {
            return Err(Error::corrupt("an update entry runs past the end of its transaction"));
        }
        while position < end {
            let offset = u64::from(view.u32(position)?);
            let size = to_usize(u64::from(view.u32(position.saturating_add(4))?), "change size")?;
            let data_start = position.saturating_add(8);
            let data_end = data_start.saturating_add(size);
            if size == 0 || data_end > end || offset.saturating_add(size as u64) > block_size {
                return Err(Error::corrupt(format!(
                    "a change of {size} bytes at {offset:#x} does not fit"
                )));
            }
            let data = slot
                .get(data_start..data_end)
                .ok_or_else(|| Error::corrupt("change outside the slot"))?;
            patches.push(Patch {
                offset: add(target, offset, "change offset")?,
                data: data.to_vec(),
            });
            position = data_end;
        }
        seen = seen.saturating_add(1);
        at = next;
    }
    if seen != update_count {
        return Err(Error::corrupt(format!(
            "{seen} update entries, the header says {update_count}"
        )));
    }
    Ok(Transaction {
        journal,
        sequence,
        patches,
    })
}

/// A volume read as if the pending transactions of its journals had reached their final place.
#[derive(Debug)]
pub(crate) struct Replayed {
    inner: Arc<dyn BlockSource>,
    /// Changes by the 4096-byte block they fall into, in the order they apply.
    blocks: BTreeMap<u64, Vec<Patch>>,
}

impl Replayed {
    pub(crate) fn new(inner: Arc<dyn BlockSource>, pending: &Pending) -> Self {
        let mut blocks: BTreeMap<u64, Vec<Patch>> = BTreeMap::new();
        for transaction in &pending.transactions {
            for patch in &transaction.patches {
                blocks
                    .entry(patch.offset & !(SLOT_SIZE - 1))
                    .or_default()
                    .push(patch.clone());
            }
        }
        Replayed { inner, blocks }
    }
}

impl BlockSource for Replayed {
    fn len(&self) -> u64 {
        self.inner.len()
    }

    fn read_exact_at(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        self.inner.read_exact_at(offset, buf)?;
        let end = offset.saturating_add(buf.len() as u64);
        let first_block = offset & !(SLOT_SIZE - 1);
        for patches in self.blocks.range(first_block..end).map(|(_, patches)| patches) {
            for patch in patches {
                // The part of the change that falls into what was read.
                let patch_end = patch.offset.saturating_add(patch.data.len() as u64);
                let (from, to) = (patch.offset.max(offset), patch_end.min(end));
                if from >= to {
                    continue;
                }
                let source = to_usize(from.saturating_sub(patch.offset), "change")?
                    ..to_usize(to.saturating_sub(patch.offset), "change")?;
                let target =
                    to_usize(from.saturating_sub(offset), "change")?..to_usize(to.saturating_sub(offset), "change")?;
                if let (Some(target), Some(source)) = (buf.get_mut(target), patch.data.get(source)) {
                    target.copy_from_slice(source);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::error::ErrorKind;
    use crate::io::MemorySource;

    const JOURNAL: u64 = 0x80_0000;

    fn put(buf: &mut [u8], offset: usize, bytes: &[u8]) {
        buf[offset..offset + bytes.len()].copy_from_slice(bytes);
    }

    /// A volume with one heartbeat slot that points to a journal whose slots are all unused.
    pub(crate) fn volume_with_journal() -> Vec<u8> {
        let mut img = vec![0u8; (JOURNAL + JOURNAL_SIZE) as usize];
        put(
            &mut img,
            JOURNAL_SIZE_FIELD as usize,
            &(JOURNAL_SIZE as u32).to_le_bytes(),
        );
        let slot = HEARTBEAT_OFFSET as usize + 5 * 4096;
        put(&mut img, slot, &0xABCD_EF02u32.to_le_bytes());
        put(&mut img, slot + HEARTBEAT_JOURNAL_OFFSET, &JOURNAL.to_le_bytes());
        let j = JOURNAL as usize;
        put(&mut img, j, &JOURNAL_MAGIC.to_le_bytes());
        put(&mut img, j + 0x20, &JOURNAL_MAGIC_2.to_le_bytes());
        for index in 1..(JOURNAL_SIZE / SLOT_SIZE) as usize {
            put(&mut img, j + index * 4096, &SLOT_DONE.to_le_bytes());
            put(&mut img, j + index * 4096 + 4, &(index as u16).to_le_bytes());
        }
        img
    }

    /// A change to a block: offset in the block and the new bytes.
    pub(crate) type Change<'a> = (u32, &'a [u8]);
    /// The changes to one block, by its volume offset.
    pub(crate) type Update<'a> = (u64, &'a [Change<'a>]);

    /// Writes a pending transaction with one lock entry and the given updates into a slot.
    pub(crate) fn write_transaction(img: &mut [u8], index: usize, sequence: u64, updates: &[Update<'_>]) {
        let s = JOURNAL as usize + index * 4096;
        img[s..s + 4096].fill(0);
        put(img, s, &SLOT_PENDING.to_le_bytes());
        put(img, s + 4, &(index as u16).to_le_bytes());
        put(img, s + 6, &0x0272u16.to_le_bytes());
        for field in [0x18, 0x40, 0x48] {
            put(img, s + field, &sequence.to_le_bytes());
        }
        // One lock entry at 0xE0, then the update entries from 0x100.
        put(img, s + 0x50, &0xE0u32.to_le_bytes());
        put(img, s + 0x58, &1u16.to_le_bytes());
        put(img, s + 0x5A, &(updates.len() as u16).to_le_bytes());
        put(img, s + 0xE0, &0xE0u32.to_le_bytes());
        put(img, s + 0xE8, &0x200u32.to_le_bytes());
        let mut at = 0x100usize;
        if !updates.is_empty() {
            put(img, s + 0x54, &(at as u32).to_le_bytes());
        }
        for (number, (target, changes)) in updates.iter().enumerate() {
            let bytes: usize = changes.iter().map(|(_, data)| 8 + data.len()).sum();
            let next = if number + 1 == updates.len() {
                0
            } else {
                at + UPDATE_HEADER_SIZE + bytes
            };
            put(img, s + at, &(at as u32).to_le_bytes());
            put(img, s + at + 4, &(next as u32).to_le_bytes());
            put(img, s + at + 8, &0x105u32.to_le_bytes());
            put(img, s + at + 0x0C, &target.to_le_bytes());
            put(img, s + at + 0x14, &(bytes as u32).to_le_bytes());
            put(img, s + at + 0x18, &4096u32.to_le_bytes());
            put(img, s + at + 0x1C, &(bytes as u32).to_le_bytes());
            let mut position = at + UPDATE_HEADER_SIZE;
            for (offset, data) in *changes {
                put(img, s + position, &offset.to_le_bytes());
                put(img, s + position + 4, &(data.len() as u32).to_le_bytes());
                put(img, s + position + 8, data);
                position += 8 + data.len();
            }
            at = position;
        }
        put(img, s + 0x30, &(at as u32).to_le_bytes());
    }

    fn replayed(img: Vec<u8>) -> Result<(Pending, Replayed)> {
        let source: Arc<dyn BlockSource> = Arc::new(MemorySource::new(img));
        let pending = scan(&*source)?;
        let view = Replayed::new(source, &pending);
        Ok((pending, view))
    }

    #[test]
    fn a_volume_without_journals_or_with_idle_ones_has_nothing_pending() {
        let empty = vec![0u8; (JOURNAL + JOURNAL_SIZE) as usize];
        let pending = scan(&MemorySource::new(empty)).unwrap();
        assert_eq!((pending.journals, pending.transactions()), (0, 0));
        let pending = scan(&MemorySource::new(volume_with_journal())).unwrap();
        assert_eq!((pending.journals, pending.transactions()), (1, 0));
    }

    #[test]
    fn pending_changes_are_applied_in_sequence_order_whatever_slot_they_are_in() {
        let mut img = volume_with_journal();
        img[0x10_0000..0x10_0008].copy_from_slice(b"original");
        // The newer transaction sits in the lower slot.
        write_transaction(&mut img, 3, 8, &[(0x10_0000, &[(0, b"NEWER"), (0xFFE, b"zz")])]);
        write_transaction(
            &mut img,
            9,
            7,
            &[(0x10_0000, &[(0, b"older!!")]), (0x10_3000, &[(0x10, b"second block")])],
        );
        let (pending, view) = replayed(img).unwrap();
        assert_eq!(pending.transactions(), 2);

        let mut buf = [0u8; 8];
        view.read_exact_at(0x10_0000, &mut buf).unwrap();
        assert_eq!(&buf, b"NEWER!!l");
        // Reads that start or end inside a change, or cross from one block to the next.
        view.read_exact_at(0x10_0003, &mut buf[..3]).unwrap();
        assert_eq!(&buf[..3], b"ER!");
        let mut tail = [0xAAu8; 4];
        view.read_exact_at(0x10_0FFD, &mut tail).unwrap();
        assert_eq!(tail, [0, b'z', b'z', 0]);
        let mut second = [0u8; 12];
        view.read_exact_at(0x10_3010, &mut second).unwrap();
        assert_eq!(&second, b"second block");
        // Everything else reads as it is on the device.
        let mut untouched = [1u8; 16];
        view.read_exact_at(0x10_2000, &mut untouched).unwrap();
        assert_eq!(untouched, [0u8; 16]);
    }

    #[test]
    fn anything_unexpected_in_a_journal_is_refused() {
        let good = {
            let mut img = volume_with_journal();
            write_transaction(&mut img, 1, 5, &[(0x10_0000, &[(8, b"data")])]);
            img
        };
        assert_eq!(scan(&MemorySource::new(good.clone())).unwrap().transactions(), 1);
        let slot = JOURNAL as usize + 4096;
        let refused = |change: &dyn Fn(&mut Vec<u8>)| {
            let mut img = good.clone();
            change(&mut img);
            scan(&MemorySource::new(img)).unwrap_err().kind()
        };

        // Unknown signatures, entry types, block sizes and flags.
        assert_eq!(refused(&|img| put(img, slot, b"XX6J")), ErrorKind::Unsupported);
        assert_eq!(
            refused(&|img| put(img, JOURNAL as usize, b"nope")),
            ErrorKind::Unsupported
        );
        assert_eq!(
            refused(&|img| put(img, slot + 0x108, &0x999u32.to_le_bytes())),
            ErrorKind::Unsupported
        );
        assert_eq!(
            refused(&|img| put(img, slot + 0xE8, &0x999u32.to_le_bytes())),
            ErrorKind::Unsupported
        );
        assert_eq!(
            refused(&|img| put(img, slot + 0x118, &512u32.to_le_bytes())),
            ErrorKind::Unsupported
        );
        assert_eq!(
            refused(&|img| put(img, slot + 6, &0u16.to_le_bytes())),
            ErrorKind::Unsupported
        );
        assert_eq!(
            refused(&|img| put(img, JOURNAL_SIZE_FIELD as usize, &0x10_0000u32.to_le_bytes())),
            ErrorKind::Unsupported
        );
        // Broken structure: entries out of place, counts that do not match, changes that do not fit.
        assert_eq!(
            refused(&|img| put(img, slot + 0x100, &0x104u32.to_le_bytes())),
            ErrorKind::Corrupt
        );
        assert_eq!(
            refused(&|img| put(img, slot + 0x5A, &2u16.to_le_bytes())),
            ErrorKind::Corrupt
        );
        assert_eq!(
            refused(&|img| put(img, slot + 0x12C, &4000u32.to_le_bytes())),
            ErrorKind::Corrupt
        );
        assert_eq!(
            refused(&|img| put(img, slot + 0x128, &4094u32.to_le_bytes())),
            ErrorKind::Corrupt
        );
        // A target outside the volume.
        assert_ne!(
            refused(&|img| put(img, slot + 0x10C, &(1u64 << 40).to_le_bytes())),
            ErrorKind::Io
        );
        // Two transactions with one sequence number.
        let mut twice = good.clone();
        write_transaction(&mut twice, 2, 5, &[(0x10_1000, &[(0, b"x")])]);
        assert_eq!(scan(&MemorySource::new(twice)).unwrap_err().kind(), ErrorKind::Corrupt);
    }
}
