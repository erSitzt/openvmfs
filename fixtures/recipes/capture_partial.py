#!/usr/bin/env python3
"""Captures the parts of a large, mostly empty device that matter: its first N MiB, the data extents ESXi listed
with `vmkfstools -t0`, and samples of the extents it marked "to be zeroed".

Usage: capture_partial.py NESTED_SH DEVICE DEVICE_SIZE HEAD_MIB OUT_IMAGE T0_FILE...
"""
import re, subprocess, sys

MIB = 1 << 20
nested, device, size, head_mib, out = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4]), sys.argv[5]
ranges = [(0, head_mib)]
line = re.compile(r"\[\s*(\d+):\s*(\d+)\] --> \[VMFS (--|Z-) \S+:\(\s*(\d+) -->\s*(\d+)\)\]")
for path in sys.argv[6:]:
    for text in open(path):
        m = line.search(text)
        if not m:
            continue
        kind, start, end = m.group(3), int(m.group(4)), int(m.group(5))
        assert start % MIB == 0 and end % MIB == 0, text
        s, e = start // MIB, end // MIB
        if kind == "--":
            ranges.append((s, e - s))
        else:
            ranges.append((s, min(2, e - s)))
            ranges.append((e - 1, 1))
ranges = sorted(set(ranges))
merged = []
for s, c in ranges:
    if merged and s <= merged[-1][0] + merged[-1][1]:
        merged[-1] = (merged[-1][0], max(merged[-1][0] + merged[-1][1], s + c) - merged[-1][0])
    else:
        merged.append((s, c))
with open(out, "wb") as f:
    f.truncate(size)
total = 0
for s, c in merged:
    src = subprocess.Popen([nested, "ssh", f"dd if={device} bs=1M skip={s} count={c} 2>/dev/null"], stdout=subprocess.PIPE)
    dst = subprocess.run(["dd", f"of={out}", "bs=1M", f"seek={s}", "conv=notrunc,sparse", "status=none"], stdin=src.stdout)
    src.wait()
    assert src.returncode == 0 and dst.returncode == 0, (s, c)
    total += c
with open(out + ".ranges", "w") as f:
    for s, c in merged:
        f.write(f"{s * MIB} {c * MIB}\n")
print(f"captured {total} MiB in {len(merged)} ranges of a {size // MIB} MiB device")
