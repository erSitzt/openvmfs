#!/usr/bin/env python3
"""Fetches the MiB-aligned parts of a device that a read trace touched but a partial image lacks.
Usage: fetch_missing.py NESTED_SH DEVICE IMAGE READS_FILE      (IMAGE.ranges is updated)"""
import subprocess, sys
MIB = 1 << 20
nested, device, image, reads_file = sys.argv[1:5]
cap = [tuple(map(int, l.split())) for l in open(image + ".ranges")]
reads = [tuple(map(int, l.split())) for l in open(reads_file) if l[:1].isdigit()]
need = set()
for o, n in reads:
    for mib in range(o // MIB, (o + n - 1) // MIB + 1):
        if not any(s <= mib * MIB and (mib + 1) * MIB <= s + c for s, c in cap):
            need.add(mib)
for mib in sorted(need):
    src = subprocess.Popen([nested, "ssh", f"dd if={device} bs=1M skip={mib} count=1 2>/dev/null"], stdout=subprocess.PIPE)
    dst = subprocess.run(["dd", f"of={image}", "bs=1M", f"seek={mib}", "conv=notrunc,sparse", "status=none"], stdin=src.stdout)
    src.wait(); assert src.returncode == 0 and dst.returncode == 0
    cap.append((mib * MIB, MIB))
cap.sort()
with open(image + ".ranges", "w") as f:
    for s, c in cap:
        f.write(f"{s} {c}\n")
print("fetched", len(need), "MiB:", sorted(need)[:12])
sys.exit(1 if need else 0)
