"""Makes ESXi grow the sub-block pool of a datastore until `.sbc.sf` needs a second pointer block in `.pb2.sf`
(more than 8 GiB, 131072 sub-blocks). Runs on the ESXi shell.

    python make_pb2.py ROOT FIRST_ROUND TARGET_SUB_BLOCKS

ESXi enlarges the pool by 512 sub-blocks only when something needs a sub-block that a file block cannot replace: a
pointer block or a directory. Small files take file blocks of 1 MiB once the pool is empty, which is slow. So every
round asks how many sub-blocks are free. With none free it creates a sparse file that needs a pointer block
(ROOT/rNNN/big, 4 KiB written behind 320 MiB), which makes the pool grow; then it writes files of 8 KiB for all free
sub-blocks but four, which the directory may need. File i of round r holds SHA-256("openvmfs-pb2-<r>-<i>") repeated
256 times, `big` holds SHA-256("openvmfs-pb2-big-<r>") 128 times at 330 MiB. Prints round, files and pool per round.
"""
import hashlib, os, re, subprocess, sys

root, first, target = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
volume = "/".join(root.split("/")[:4])


def pool():
    out = subprocess.Popen(["vmkfstools", "-Ph", "-v10", volume], stdout=subprocess.PIPE).communicate()[0].decode()
    m = re.search(r"Sub Blocks \(max/free\): (\d+)/(\d+)", out)
    return int(m.group(1)), int(m.group(2))


r = first
while True:
    total, free = pool()
    if total >= target and free <= 4:
        break
    p = "%s/r%03d" % (root, r)
    os.makedirs(p)
    if free <= 4:
        f = open(p + "/big", "wb")
        f.truncate(400 << 20)
        f.seek(330 << 20)
        f.write(hashlib.sha256(("openvmfs-pb2-big-%d" % r).encode()).digest() * 128)
        f.close()
        total, free = pool()
    count = max(0, min(free - 4, 600))
    for i in range(count):
        f = open("%s/f%03d" % (p, i), "wb")
        f.write(hashlib.sha256(("openvmfs-pb2-%d-%d" % (r, i)).encode()).digest() * 256)
        f.close()
    sys.stdout.write("%d %d %d/%d\n" % ((r, count) + pool()))
    sys.stdout.flush()
    r += 1
