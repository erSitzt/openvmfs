#!/usr/bin/env python
"""Reference data for OpenVmfs fixtures. Runs on the ESXi shell (Python 3.8) and on any other Python 3.

Every 512-byte sector written by this script describes itself, so that a scan of a raw volume image shows where
each sector really is, and a reader that returns the wrong block is caught.

Pattern sector (written into virtual disks and files):
    0   8  magic  b"OVMFSPAT"
    8   4  disk id
    12  4  generation (which write pass produced it)
    16  8  sector number inside the disk or file
    24  4  CRC-32 of bytes 0..24
    28  484  filler: the byte (sector * 7 + disk id) & 0xFF

Junk sector (written to fill a datastore before the real files are created, so that stale data is recognisable):
    0   8  magic  b"OVMFSJNK"
    8   8  sector number inside the junk file
    16  496  filler: 0xEE

Commands:
    pattern.py junk PATH MIB                      write MIB mebibytes of junk sectors to a new file
    pattern.py junkfill PATH MIB                  overwrite the first MIB mebibytes of an existing file with junk
    pattern.py write PATH DISK_ID GEN START COUNT [START COUNT ...]
                                                  write pattern sectors into an existing file (ranges in sectors)
    pattern.py expect SIZE_BYTES DISK_ID GEN START COUNT [...]
                                                  print the SHA-256 a file of SIZE_BYTES has when it is zero
                                                  except for these pattern ranges
"""
import hashlib
import os
import struct
import sys
import zlib

SECTOR = 512
CHUNK_SECTORS = 2048


def pattern_sector(disk_id, generation, sector):
    head = b"OVMFSPAT" + struct.pack("<IIQ", disk_id, generation, sector)
    head += struct.pack("<I", zlib.crc32(head) & 0xFFFFFFFF)
    return head + bytes([(sector * 7 + disk_id) & 0xFF]) * (SECTOR - len(head))


def junk_chunk(first_sector, count):
    chunk = bytearray(b"\xEE" * (SECTOR * count))
    for i in range(count):
        chunk[i * SECTOR : i * SECTOR + 16] = b"OVMFSJNK" + struct.pack("<Q", first_sector + i)
    return bytes(chunk)


def ranges_from(args):
    numbers = [int(a, 0) for a in args]
    if len(numbers) % 2:
        raise SystemExit("ranges are START COUNT pairs")
    return list(zip(numbers[0::2], numbers[1::2]))


def cmd_junk(path, mib, existing=False):
    fd = os.open(path, os.O_WRONLY if existing else os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        for chunk in range(mib):
            os.write(fd, junk_chunk(chunk * CHUNK_SECTORS, CHUNK_SECTORS))
        os.fsync(fd)
    finally:
        os.close(fd)
    print("junk: wrote %d MiB to %s" % (mib, path))


def cmd_write(path, disk_id, generation, ranges):
    fd = os.open(path, os.O_WRONLY)
    try:
        for start, count in ranges:
            done = 0
            while done < count:
                n = min(CHUNK_SECTORS, count - done)
                data = b"".join(pattern_sector(disk_id, generation, start + done + i) for i in range(n))
                os.pwrite(fd, data, (start + done) * SECTOR)
                done += n
        os.fsync(fd)
    finally:
        os.close(fd)
    print("write: %s disk %d generation %d ranges %s" % (path, disk_id, generation, ranges))


def cmd_expect(size, disk_id, generation, ranges):
    if size % SECTOR:
        raise SystemExit("size must be a multiple of 512")
    written = {}
    for start, count in ranges:
        for sector in range(start, start + count):
            written[sector] = True
    digest = hashlib.sha256()
    zero_chunk = bytes(SECTOR * CHUNK_SECTORS)
    total = size // SECTOR
    sector = 0
    starts = sorted(written)
    index = 0
    while sector < total:
        if index < len(starts) and starts[index] == sector:
            digest.update(pattern_sector(disk_id, generation, sector))
            sector += 1
            index += 1
            continue
        stop = starts[index] if index < len(starts) else total
        gap = stop - sector
        while gap > 0:
            n = min(gap, CHUNK_SECTORS)
            digest.update(zero_chunk[: n * SECTOR])
            gap -= n
            sector += n
    print(digest.hexdigest())


def main(argv):
    if len(argv) >= 4 and argv[1] in ("junk", "junkfill"):
        cmd_junk(argv[2], int(argv[3]), existing=argv[1] == "junkfill")
    elif len(argv) >= 7 and argv[1] == "write":
        cmd_write(argv[2], int(argv[3]), int(argv[4]), ranges_from(argv[5:]))
    elif len(argv) >= 5 and argv[1] == "expect":
        cmd_expect(int(argv[2], 0), int(argv[3]), int(argv[4]), ranges_from(argv[5:]))
    else:
        sys.stderr.write(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
