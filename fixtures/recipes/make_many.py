"""Creates many small files on a datastore, to make ESXi grow its descriptor and sub-block pools. Runs on the ESXi shell.

    python make_many.py ROOT FIRST_DIR LAST_DIR PER_DIR

Writes ROOT/dNNN/fNNNN, 8 KiB each. File number n = dir * PER_DIR + index holds SHA-256("openvmfs-many-<n>") repeated
256 times, so a reader can check any file without a list of hashes. Slow: about 1000 files per 40 minutes on a
nested host once the sub-blocks are used up, and about 0.6 MiB of datastore space per file from then on.
"""
import hashlib, os, sys

root, first, last, per = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
for d in range(first, last):
    p = "%s/d%03d" % (root, d)
    if not os.path.isdir(p):
        os.makedirs(p)
    for i in range(per):
        n = d * per + i
        f = open("%s/f%04d" % (p, i), "wb")
        f.write(hashlib.sha256(("openvmfs-many-%d" % n).encode()).digest() * 256)
        f.close()
    sys.stdout.write("%d\n" % d)
    sys.stdout.flush()
