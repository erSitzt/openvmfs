#!/usr/bin/env python3
"""Writes IMAGE.ranges for a full local image: the MiB-aligned ranges a read trace touched, plus extra ranges.

Together with pack_partial.py this turns a full image into a small partial fixture that contains exactly what the
tests read. Tests open it through a source that fails on reads outside these ranges.

Usage: subset_image.py IMAGE READS_FILE [OFFSET:LENGTH ...]     (READS_FILE: "OFFSET LENGTH" per line)
"""
import sys

MIB = 1 << 20
image, reads_file = sys.argv[1], sys.argv[2]
wanted = [tuple(map(int, line.split())) for line in open(reads_file) if line[:1].isdigit()]
wanted += [tuple(int(part, 0) for part in arg.split(":")) for arg in sys.argv[3:]]
mibs = set()
for offset, length in wanted:
    mibs.update(range(offset // MIB, (offset + length - 1) // MIB + 1))
ranges = []
for mib in sorted(mibs):
    if ranges and ranges[-1][0] + ranges[-1][1] == mib:
        ranges[-1][1] += 1
    else:
        ranges.append([mib, 1])
with open(image + ".ranges", "w") as out:
    for start, count in ranges:
        out.write("%d %d\n" % (start * MIB, count * MIB))
print("%d ranges, %d MiB" % (len(ranges), sum(count for _, count in ranges)))
