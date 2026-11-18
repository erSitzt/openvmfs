#!/usr/bin/env python3
"""Repeats the crash-consistency probe in the backup order (see docs/consistency.md).

Setting: an ESXi host running as a VM ("outer VM") has a small datastore on one of its virtual disks. On that
datastore a guest VM has a source disk full of pattern data (first disk, 64 MiB) and a thin target disk (second
disk), and is logged in on its console as root.

Each run:
  1. the guest starts copying the next 64 MiB from the source to the target in 256 KiB writes, each followed by
     fsync and a short pause, so that the copy takes a few seconds and allocates new blocks all the time;
  2. at the same moment a VM snapshot of the guest is requested (it freezes the disk as it is when it lands);
  3. as soon as that returns, a snapshot of the outer VM is taken: the "storage snapshot";
  4. the frozen datastore disk is copied, the outer snapshot removed, and the copy downloaded without any ESXi host
     ever seeing it;
  5. OpenVmfs reads the disk as of step 2 from that copy; its SHA-256 and some facts about the frozen image are
     appended to <workdir>/runs.jsonl.

Phase "compare", after the last run: the guest is powered off and ESXi consolidates every snapshot level into a
clone. A run passes when the hash from step 5 equals the hash of ESXi's clone of the same level.

Usage:
  consistency_probe.py runs    --runs N [--start-run K] <common options>
  consistency_probe.py compare <common options>

Common options: --vcenter-env FILE --outer-vm PATH --outer-ds NAME --outer-disk DIR/NAME.vmdk --nested NESTED_SH
                --guest-vm NAME --datastore LABEL --target DIR/NAME --clone-datastore LABEL --cli OPENVMFS
                --workdir DIR [--settle SECONDS] [--lead SECONDS] [--source DIR/NAME] [--spare START END]
"""
import argparse
import hashlib
import json
import os
import struct
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CHUNKS_PER_RUN = 256  # of 256 KiB each: 64 MiB
MIB = 1 << 20


def main():
    p = argparse.ArgumentParser()
    p.add_argument("phase", choices=("runs", "compare"))
    for name in ("vcenter-env", "outer-vm", "outer-ds", "outer-disk", "nested", "guest-vm", "datastore", "target",
                 "clone-datastore", "cli", "workdir"):
        p.add_argument("--" + name, required=True)
    p.add_argument("--runs", type=int, default=1)
    p.add_argument("--start-run", type=int, default=1)
    p.add_argument("--settle", type=float, default=0.0, help="seconds between the VM snapshot and the outer snapshot")
    p.add_argument("--lead", type=float, default=0.5, help="seconds the guest writes before the VM snapshot (plus 0-3)")
    p.add_argument("--source", default="probe/src", help="the pattern source disk, as DIR/NAME")
    p.add_argument("--spare", type=int, nargs=2, default=(690 * MIB, 698 * MIB),
                   help="byte range of the target that holds unrelated data and is ignored by the order check")
    a = p.parse_args()
    os.makedirs(a.workdir, exist_ok=True)
    log_path = os.path.join(a.workdir, "runs.jsonl")
    env = dict(os.environ, OPENVMFS_VCENTER_ENV=os.path.expanduser(a.vcenter_env))
    vcenter = [sys.executable, "-I", os.path.join(REPO, "tools", "vcenter.py")]

    def vc(*args):
        return subprocess.run([*vcenter, *args], env=env, capture_output=True, text=True)

    def nested(*args):
        return subprocess.run([a.nested, *args], capture_output=True, text=True)

    def cli(*args):
        return subprocess.run([a.cli, *args], capture_output=True)

    def level_name(level, suffix):
        return a.target + suffix if level == 0 else "%s-%06d%s" % (a.target, level, suffix)

    zero_mib = hashlib.sha256(bytes(MIB)).hexdigest()

    def chunk_hashes(image, descriptor):
        out = cli("vmdk", "hash", "--chunks", image, descriptor)
        if out.returncode:
            return None, out.stderr.decode(errors="replace").strip()[-200:]
        return {int(offset, 16): digest for offset, digest in (line.split() for line in out.stdout.decode().split("\n")
                                                                if line.strip())}, None

    def write_order(image, descriptor, run):
        """Classifies every MiB of the target as written in full, partly or not at all, and checks the order."""
        source, error = chunk_hashes(image, a.source + ".vmdk")
        target, error2 = chunk_hashes(image, descriptor)
        if source is None or target is None:
            return {"error": error or error2}
        source = [source[m * MIB] for m in range(len(source))]
        states = []
        for m in range(run * CHUNKS_PER_RUN // 4):
            digest = target.get(m * MIB, zero_mib)
            states.append("full" if digest == source[m % len(source)] else "none" if digest == zero_mib else "part")
        full = next((m for m, state in enumerate(states) if state != "full"), len(states))
        rest = states[full:]
        # After the unbroken prefix: at most one MiB that is partly written, then nothing.
        in_order = all(state == "none" for state in rest[1:]) and (not rest or rest[0] != "full")
        beyond = sum(1 for offset, digest in target.items()
                     if offset >= len(states) * MIB and digest != zero_mib and not a.spare[0] <= offset < a.spare[1])
        return {"full_mib": full, "partly_written_mib": states.count("part"), "in_order": in_order,
                "data_beyond_the_copy": beyond}

    def sesparse_summary(delta):
        """Facts about a delta file as it is on disk: headers, journal, and what its tables hold."""
        header = struct.unpack_from("<26Q", delta, 0)
        _magic, _free_tables, _next_txn, replay = struct.unpack_from("<4Q", delta, 512)
        journal_offset, journal_size, directory, tables, grains = header[14], header[15], header[16], header[18], header[24]
        journal = delta[journal_offset * 512:(journal_offset + journal_size) * 512]
        allocated = 0
        highest = -1
        if len(delta) >= grains * 512:
            entries = struct.unpack_from("<%dQ" % ((header[2] + 32767) // 32768), delta, directory * 512)
            for entry in entries:
                if entry:
                    table = struct.unpack_from("<4096Q", delta, (tables + (entry & 0xFFFFFFFF) * 64) * 512)
                    for grain in table:
                        if grain >> 60 == 3:
                            allocated += 1
                            highest = max(highest, ((grain >> 48) & 0xFFF) | ((grain & 0xFFFFFFFFFFFF) << 12))
        return {"length": len(delta), "replay_journal": replay,
                "journal_sectors_in_use": sum(1 for i in range(0, len(journal), 512) if any(journal[i:i + 512])),
                "allocated_grains": allocated,
                "grains_beyond_the_file": max(0, grains * 512 + (highest + 1) * 4096 - len(delta)) // 4096}

    def one_run(run):
        level = run - 1
        start, end = level * CHUNKS_PER_RUN, run * CHUNKS_PER_RUN
        # Type the command first; Enter and the snapshot request then go out together.
        nested("govc", "vm.keystrokes", "-vm", a.guest_vm, "-s",
               "i=%d; while [ $i -lt %d ]; do dd if=/dev/sda of=/dev/sdb bs=256k skip=$((i%%256)) seek=$i count=1 "
               "conv=notrunc,fsync 2>/dev/null; usleep 20000; i=$((i+1)); done; echo RUN-%d-DONE" % (start, end, run))
        # A key sent while the snapshot is being taken gets lost, so Enter goes first. The lead differs from run to
        # run, so that the snapshots land at different points of the copy.
        nested("govc", "vm.keystrokes", "-vm", a.guest_vm, "-c", "KEY_ENTER")
        t0 = time.time()
        time.sleep(a.lead + (level % 4))
        r1 = nested("govc", "snapshot.create", "-vm", a.guest_vm, "-m=false", "-q=false", "probe%d" % run)
        t1 = time.time()
        if r1.returncode:
            raise SystemExit("VM snapshot failed in run %d: %s" % (run, r1.stderr[-300:]))
        if a.settle:
            time.sleep(a.settle)
        r2 = vc("snapshot.create", "-vm", a.outer_vm, "-m=false", "-q=false", "probe%d" % run)
        t2 = time.time()
        if r2.returncode:
            raise SystemExit("outer snapshot failed in run %d: %s" % (run, r2.stderr[-300:]))
        copy = "%s/probe-copy-%d.vmdk" % (os.path.dirname(a.outer_disk), run)
        image = os.path.join(a.workdir, "probe-%d.img" % run)
        try:
            time.sleep(8)  # let the guest finish its loop
            copied = vc("datastore.cp", "-ds", a.outer_ds, a.outer_disk, copy)
        finally:
            removed = vc("snapshot.remove", "-vm", a.outer_vm, "probe%d" % run)
        if copied.returncode or removed.returncode:
            raise SystemExit("copy or snapshot removal failed in run %d: %s %s"
                             % (run, copied.stderr[-200:], removed.stderr[-200:]))
        download = subprocess.Popen([*vcenter, "datastore.download", "-ds", a.outer_ds,
                                     copy.replace(".vmdk", "-flat.vmdk"), "-"],
                                    env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        subprocess.run(["dd", "of=" + image, "bs=1M", "status=none"], stdin=download.stdout, check=True)
        if download.wait():
            raise SystemExit("download failed in run %d" % run)
        vc("datastore.rm", "-ds", a.outer_ds, copy)

        descriptor = level_name(level, ".vmdk")
        hashed = cli("vmdk", "hash", image, descriptor)
        info = cli("vmdk", "info", image, descriptor).stdout.decode(errors="replace")
        volume = cli("info", image).stdout.decode(errors="replace")
        result = {
            "run": run,
            "level": level,
            "descriptor": descriptor,
            "ours": hashed.stdout.split()[0].decode() if hashed.returncode == 0
            else "ERROR: " + hashed.stderr.decode(errors="replace").strip()[-200:],
            "data_bytes": next((int(line.split()[1]) for line in info.splitlines() if line.startswith("data:")), None),
            "active_heartbeats": next((line.split(":", 1)[1].strip() for line in volume.splitlines()
                                       if line.startswith("active heartbeats")), None),
            "seconds_enter_to_vm_snapshot_done": round(t1 - t0, 2),
            "seconds_vm_snapshot_done_to_outer_snapshot_done": round(t2 - t1, 2),
        }
        # The guest writes in order and waits for each write to reach the disk. Whatever a crash-consistent image
        # holds must therefore be an unbroken prefix of the copy.
        result["frozen_order"] = write_order(image, descriptor, run)
        result["active_order"] = write_order(image, level_name(run, ".vmdk"), run)
        # The delta that was closed by the VM snapshot (if the frozen level has one), and the one opened by it.
        for key, delta_level in (("frozen_delta", level), ("active_delta", run)):
            if delta_level == 0:
                continue
            name = level_name(delta_level, "-sesparse.vmdk")
            delta = cli("cat", image, name)
            if delta.returncode or len(delta.stdout) < 1024:
                result[key] = {"error": delta.stderr.decode(errors="replace").strip()[-160:]}
                continue
            result[key] = dict(sesparse_summary(delta.stdout), file=name)
            with open(os.path.join(a.workdir, "run%d-%s.head" % (run, key)), "wb") as out:
                out.write(delta.stdout[:12 * MIB])
        with open(log_path, "a") as log:
            log.write(json.dumps(result) + "\n")
        os.remove(image)
        print("run %d: level %d frozen with %s MiB of data, heartbeats %s, ours %s" % (
            run, level, None if result["data_bytes"] is None else result["data_bytes"] // MIB,
            result["active_heartbeats"], result["ours"][:24]), flush=True)

    if a.phase == "runs":
        for run in range(a.start_run, a.start_run + a.runs):
            one_run(run)
        return 0

    # compare: ESXi's own consolidation of every level, after the guest has been powered off.
    results = [json.loads(line) for line in open(log_path)]
    nested("govc", "vm.keystrokes", "-vm", a.guest_vm, "-s", "poweroff")
    nested("govc", "vm.keystrokes", "-vm", a.guest_vm, "-c", "KEY_ENTER")
    time.sleep(40)
    clone_dir = "/vmfs/volumes/%s/probe-clones" % a.clone_datastore
    failures = 0
    for result in results:
        source = "/vmfs/volumes/%s/%s" % (a.datastore, result["descriptor"])
        out = nested("ssh", "rm -rf %s; mkdir -p %s && vmkfstools -i %s -d thin %s/c.vmdk >/dev/null 2>&1 && "
                            "sha256sum %s/c-flat.vmdk; rm -rf %s" % (clone_dir, clone_dir, source, clone_dir,
                                                                    clone_dir, clone_dir))
        result["esxi"] = out.stdout.split()[0] if out.stdout.strip() else "ERROR: " + out.stderr.strip()[-200:]
        result["identical"] = result["esxi"] == result["ours"]
        failures += not result["identical"]
        print("level %d: ESXi %s  %s" % (result["level"], result["esxi"][:16],
                                         "identical" if result["identical"] else "DIFFERENT"), flush=True)
    sizes = nested("ssh", "ls -l /vmfs/volumes/%s/%s*-sesparse.vmdk" % (a.datastore, a.target)).stdout
    with open(os.path.join(a.workdir, "results.json"), "w") as out:
        json.dump({"runs": results, "final_delta_listing": sizes.splitlines()}, out, indent=1)
    print("%d runs, %d identical, %d different" % (len(results), len(results) - failures, failures))
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
