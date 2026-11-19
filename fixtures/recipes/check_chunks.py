#!/usr/bin/env python3
"""check_chunks.py CLI IMAGE DESCRIPTOR T0_FILE DISK_ID GENERATION SECTOR COUNT [SECTOR COUNT ...]
Checks a huge sparse disk without hashing its zeros: per-MiB hashes of the data against the pattern that was written,
and the data extents against ESXi's block map
(T0_FILE is the output of `vmkfstools -t0`). The arguments after T0_FILE are the ones `pattern.py write` was given."""
import hashlib, re, subprocess, sys
import os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import pattern
cli, image, desc, t0, disk_id, gen = sys.argv[1:7]
pairs = [int(v) for v in sys.argv[7:]]
written = {}
for first, count in zip(pairs[::2], pairs[1::2]):
    for s in range(first, first + count):
        written.setdefault(s // 2048, {})[s % 2048] = pattern.pattern_sector(int(disk_id), int(gen), s)
expected = {}
for mib, sectors in written.items():
    buf = bytearray(1 << 20)
    for i, data in sectors.items():
        buf[i * 512:(i + 1) * 512] = data
    expected[mib << 20] = hashlib.sha256(buf).hexdigest()
out = subprocess.run([cli, "vmdk", "hash", "--chunks", image, desc], capture_output=True, text=True)
if out.returncode:
    sys.exit("ERROR " + out.stderr.strip())
ours = {}
for line in out.stdout.splitlines():
    parts = line.split()
    if len(parts) == 2 and len(parts[1]) == 64 or len(parts) == 2 and len(parts[0]) == 64:
        h, off = (parts[1], parts[0]) if len(parts[1]) == 64 else (parts[0], parts[1])
        try: ours[int(off, 0)] = h
        except ValueError: pass
bad = [hex(o) for o in sorted(set(ours) | set(expected)) if ours.get(o) != expected.get(o)]
print("%s: %d data MiB, %d expected, %s" % (desc, len(ours), len(expected), "content identical to what was written" if not bad else "DIFFERENT at " + ",".join(bad[:6])))
esxi = [(int(a), int(b)) for a, b in re.findall(r"\[\s*(\d+):\s*(\d+)\] --> \[VMFS --", open(t0).read())]
m = subprocess.run([cli, "vmdk", "map", image, desc], capture_output=True, text=True).stdout
mine = [(int(a, 16), int(b, 16)) for a, b in re.findall(r"^\s*(0x[0-9a-f]+)\s+(0x[0-9a-f]+)\s+data", m, re.M)]
print("   block map: %s (%d extents)" % ("identical to ESXi" if mine == esxi else "DIFFERENT %s vs %s" % (mine, esxi), len(mine)))
sys.exit(1 if bad or mine != esxi else 0)
