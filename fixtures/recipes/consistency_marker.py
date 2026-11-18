#!/usr/bin/env python3
"""Crash-consistency probe WITHOUT a VM snapshot (see docs/consistency.md).

Setting as for consistency_probe.py: an ESXi host running as a VM ("outer VM") with a small datastore, on it a guest
with a 64 MiB pattern source disk (first disk) and a thin target disk (second disk) whose first sector holds "0".

The guest alternates two kinds of writes and waits for each to reach the disk (fsync):
  - chunk i: 256 KiB of pattern data into a part of the thin disk that was never written, so new blocks are needed;
  - the marker: sector 0, which exists already, is overwritten in place with the number i.

While it does that, a snapshot of the outer VM is taken: a "storage snapshot" of a datastore whose VM is writing,
with no VM snapshot before it. The frozen datastore disk is copied and downloaded, and OpenVmfs reads the target
disk from it.

The marker in the frozen image says K. The guest wrote chunk K before it wrote that marker, so a crash-consistent
image holds chunks 1..K completely, perhaps chunk K+1, and nothing after it. If a chunk up to K is missing, the image
shows a later write without an earlier one: block allocation lagged behind in-place writes, and a filesystem in the
guest could be damaged in the same way.

Usage: consistency_marker.py --runs N [--start-run K] --vcenter-env FILE --outer-vm PATH --outer-ds NAME
                             --outer-disk DIR/NAME.vmdk --nested NESTED_SH --guest-vm NAME --target DIR/NAME
                             --source DIR/NAME --cli OPENVMFS --workdir DIR
"""
import argparse
import json
import os
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CHUNK = 256 << 10
PER_RUN = 300


def main():
    p = argparse.ArgumentParser()
    for name in ("vcenter-env", "outer-vm", "outer-ds", "outer-disk", "nested", "guest-vm", "target", "source", "cli",
                 "workdir"):
        p.add_argument("--" + name, required=True)
    p.add_argument("--runs", type=int, required=True)
    p.add_argument("--start-run", type=int, default=1)
    a = p.parse_args()
    os.makedirs(a.workdir, exist_ok=True)
    env = dict(os.environ, OPENVMFS_VCENTER_ENV=os.path.expanduser(a.vcenter_env))
    vcenter = [sys.executable, "-I", os.path.join(REPO, "tools", "vcenter.py")]

    def vc(*args):
        return subprocess.run([*vcenter, *args], env=env, capture_output=True, text=True)

    def nested(*args):
        return subprocess.run([a.nested, *args], capture_output=True, text=True)

    failures = 0
    for run in range(a.start_run, a.start_run + a.runs):
        first, last = (run - 1) * PER_RUN + 1, run * PER_RUN
        nested("govc", "vm.keystrokes", "-vm", a.guest_vm, "-s",
               "i=%d; while [ $i -le %d ]; do dd if=/dev/sda of=/dev/sdb bs=256k skip=$((i%%256)) seek=$((i+4)) "
               "count=1 conv=notrunc,fsync 2>/dev/null; echo $i | dd of=/dev/sdb bs=512 count=1 "
               "conv=notrunc,fsync,sync 2>/dev/null; i=$((i+1)); done; echo RUN-%d-DONE" % (first, last, run))
        nested("govc", "vm.keystrokes", "-vm", a.guest_vm, "-c", "KEY_ENTER")
        t0 = time.time()
        time.sleep(1 + (run - 1) % 4)
        snapshot = vc("snapshot.create", "-vm", a.outer_vm, "-m=false", "-q=false", "marker%d" % run)
        t1 = time.time()
        if snapshot.returncode:
            raise SystemExit("outer snapshot failed in run %d: %s" % (run, snapshot.stderr[-300:]))
        copy = "%s/marker-copy-%d.vmdk" % (os.path.dirname(a.outer_disk), run)
        image = os.path.join(a.workdir, "marker-%d.img" % run)
        try:
            time.sleep(25)  # let the guest finish its loop
            copied = vc("datastore.cp", "-ds", a.outer_ds, a.outer_disk, copy)
        finally:
            removed = vc("snapshot.remove", "-vm", a.outer_vm, "marker%d" % run)
        if copied.returncode or removed.returncode:
            raise SystemExit("copy or snapshot removal failed in run %d" % run)
        download = subprocess.Popen([*vcenter, "datastore.download", "-ds", a.outer_ds,
                                     copy.replace(".vmdk", "-flat.vmdk"), "-"],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        subprocess.run(["dd", "of=" + image, "bs=1M", "status=none"], stdin=download.stdout, check=True)
        if download.wait():
            raise SystemExit("download failed in run %d" % run)
        vc("datastore.rm", "-ds", a.outer_ds, copy)

        exported = {}
        for name in (a.target, a.source):
            out = os.path.join(a.workdir, os.path.basename(name) + ".raw")
            done = subprocess.run([a.cli, "vmdk", "export", image, name + ".vmdk", out], capture_output=True, text=True)
            exported[name] = out if done.returncode == 0 else None
            if done.returncode:
                print("run %d: cannot read %s: %s" % (run, name, done.stderr.strip()[-200:]), flush=True)
        result = {"run": run, "seconds_guest_wrote_before_the_snapshot": round(t1 - t0, 2)}
        if all(exported.values()):
            with open(exported[a.target], "rb") as target, open(exported[a.source], "rb") as source:
                marker = int(target.read(512).split(b"\n", 1)[0] or b"0")
                states = {}
                for i in range(1, last + 1):
                    target.seek((i + 4) * CHUNK)
                    source.seek((i % 256) * CHUNK)
                    got, want = target.read(CHUNK), source.read(CHUNK)
                    states[i] = "full" if got == want else "none" if not any(got) else "part"
            missing = [i for i in range(1, marker + 1) if states[i] != "full"]
            beyond = [i for i in range(marker + 2, last + 1) if states[i] != "none"]
            result.update({
                "marker": marker,
                "chunks_complete": sum(1 for state in states.values() if state == "full"),
                "chunk_after_the_marker": states.get(marker + 1, "-"),
                "missing_before_the_marker": missing[:20],
                "data_beyond_the_marker": beyond[:20],
                "consistent": not missing and not beyond,
            })
            failures += not result["consistent"]
        else:
            result["consistent"] = False
            failures += 1
        with open(os.path.join(a.workdir, "marker-runs.jsonl"), "a") as log:
            log.write(json.dumps(result) + "\n")
        print(json.dumps(result), flush=True)
        for path in [image, *filter(None, exported.values())]:
            os.remove(path)
    print("%d runs, %d inconsistent" % (a.runs, failures))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
