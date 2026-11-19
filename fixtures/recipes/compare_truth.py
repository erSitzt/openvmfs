#!/usr/bin/env python3
"""Compares what OpenVmfs reads from an image with what ESXi recorded about the same datastore.

  compare_truth.py OPENVMFS_CLI IMAGE[,IMAGE...] TRUTH_DIR

TRUTH_DIR holds sha256.txt (output of `sha256sum` on ESXi, paths relative to the datastore) and, for virtual disks,
NAME.t0.txt (output of `vmkfstools -t0`). Every file is hashed as a file; every `-flat.vmdk` is also read as a
virtual disk through its descriptor, and its data extents are compared with ESXi's block map. Exit code 1 on any
difference.
"""
import os, re, subprocess, sys

cli, image, truth = sys.argv[1:4]
failures = 0


def run(*args):
    out = subprocess.run([cli, *args], capture_output=True, text=True)
    return out.stdout if out.returncode == 0 else "ERROR " + out.stderr.strip()[-200:]


def check(what, ours, theirs):
    global failures
    ok = ours == theirs
    failures += not ok
    print("%-9s %s%s" % ("identical" if ok else "DIFFERENT", what, "" if ok else "\n   ours:  %s\n   ESXi:  %s" % (str(ours)[:160], str(theirs)[:160])))


for line in open(os.path.join(truth, "sha256.txt")):
    digest, path = line.split(None, 1)
    path = path.strip()
    check("file " + path, run("hash", image, path).split(" ")[0], digest)
    if path.endswith("-flat.vmdk"):
        descriptor = path[:-len("-flat.vmdk")] + ".vmdk"
        check("disk " + descriptor, run("vmdk", "hash", image, descriptor).split(" ")[0], digest)
        t0 = os.path.join(truth, os.path.basename(descriptor)[:-5] + ".t0.txt")
        if os.path.exists(t0):
            # [ file offset: length] --> [VMFS -- LVID:.../1:( device start --> device end)]
            esxi = [(int(m.group(1)), int(m.group(2)), int(m.group(3))) for m in
                    re.finditer(r"\[\s*(\d+):\s*(\d+)\] --> \[VMFS -- \S+:\(\s*(\d+) -->", open(t0).read())]
            ours = []
            for row in run("vmdk", "map", image, descriptor).splitlines()[1:]:
                f = row.split()
                if len(f) >= 5 and f[2] == "data":
                    ours.append((int(f[0], 16), int(f[1], 16), int(f[4], 16)))
            # ESXi lists every piece; neighbours that are adjacent in file and on the device are one extent for us.
            merged = []
            for offset, length, device in esxi:
                if merged and merged[-1][0] + merged[-1][1] == offset and merged[-1][2] + merged[-1][1] == device:
                    merged[-1] = (merged[-1][0], merged[-1][1] + length, merged[-1][2])
                else:
                    merged.append((offset, length, device))
            check("block map " + descriptor + " (%d data extents)" % len(merged), ours, merged)
print("%d difference(s)" % failures)
sys.exit(1 if failures else 0)
