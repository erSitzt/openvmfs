#!/usr/bin/env python3
"""Packs a partially captured device image for the repository.

A large, mostly empty device is captured only where it matters (capture_partial.py, fetch_missing.py). This script
blanks given byte strings (for example the host's IP address in its heartbeat slot), then writes

    NAME.ranges      "size BYTES", then one "OFFSET LENGTH" line per captured range
    NAME.parts.zst   the bytes of all captured ranges, concatenated in that order, zstd-compressed

Usage: pack_partial.py IMAGE OUT_PREFIX [--blank TEXT]...
"""
import os
import subprocess
import sys

image, prefix = sys.argv[1], sys.argv[2]
blank = [sys.argv[i + 1].encode() for i, a in enumerate(sys.argv) if a == "--blank"]
ranges = [tuple(map(int, line.split())) for line in open(image + ".ranges")]
size = os.path.getsize(image)

blanked = []
with open(image, "r+b") as f:
    for offset, length in ranges:
        f.seek(offset)
        data = f.read(length)
        for needle in blank:
            at = data.find(needle)
            while at != -1:
                f.seek(offset + at)
                f.write(bytes(len(needle)))
                blanked.append(offset + at)
                at = data.find(needle, at + 1)

with open(prefix + ".ranges", "w") as out:
    out.write("size %d\n" % size)
    for offset, length in ranges:
        out.write("%d %d\n" % (offset, length))

zstd = subprocess.Popen(["zstd", "-q", "-19", "-T0", "-f", "-o", prefix + ".parts.zst"], stdin=subprocess.PIPE)
with open(image, "rb") as f:
    for offset, length in ranges:
        f.seek(offset)
        remaining = length
        while remaining:
            chunk = f.read(min(remaining, 8 << 20))
            zstd.stdin.write(chunk)
            remaining -= len(chunk)
zstd.stdin.close()
if zstd.wait() != 0:
    raise SystemExit("zstd failed")
print("packed %d ranges, %d MiB; blanked %d place(s) at %s" % (
    len(ranges), sum(l for _, l in ranges) >> 20, len(blanked), [hex(b) for b in blanked]))
