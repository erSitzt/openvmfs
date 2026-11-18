"""Prints what an independent reader (dissect.vmfs) sees in a VMFS image, in the format of `openvmfs-testkit`.

Commands:
  manifest IMAGE [MAX_HASH_BYTES]   one line per entry: KIND SIZE SHA256|- PATH   (tab separated)
  chunks IMAGE PATH OFFSET...       one line per offset: OFFSET SHA256 of the MiB at that offset
"""

import hashlib
import os
import sys

from dissect.util.stream import RangeStream
from dissect.vmfs.lvm import LVM
from dissect.vmfs.vmfs import VMFS

MIB = 1 << 20


def open_fs(image):
    fh = open(image, "rb")
    # For an image of a whole disk, ORACLE_PARTITION_OFFSET is where the VMFS partition starts.
    offset = int(os.environ.get("ORACLE_PARTITION_OFFSET", "0"), 0)
    if offset:
        size = fh.seek(0, os.SEEK_END) - offset
        fh = RangeStream(fh, offset, size)
    lvm = LVM(fh)
    return VMFS(lvm.volumes[0].open())


def kind_of(fd):
    if fd.is_symlink():
        return "l"
    if fd.is_rdm():
        return "r"
    if fd.is_system():
        return "s"
    if fd.is_dir():
        return "d"
    return "-"


def sha256_of(fd):
    digest = hashlib.sha256()
    with fd.open() as stream:
        while True:
            buf = stream.read(MIB)
            if not buf:
                break
            digest.update(buf)
    return digest.hexdigest()


def walk(fs, fd, prefix, limit, out):
    entries = sorted(fd.iterdir(), key=lambda e: e.name)
    for entry in entries:
        if entry.name in (".", ".."):
            continue
        child = entry.file_descriptor
        path = f"{prefix}/{entry.name}"
        kind = kind_of(child)
        is_dir = child.is_dir()
        if kind == "l":
            digest = hashlib.sha256(child.link.encode()).hexdigest()
        elif kind == "r" or is_dir or child.size > limit:
            digest = "-"
        else:
            digest = sha256_of(child)
        out.write(f"{kind}\t{child.size}\t{digest}\t{path}\n")
        if is_dir and kind != "s":
            walk(fs, child, path, limit, out)


def main(argv):
    if len(argv) >= 3 and argv[1] == "manifest":
        limit = int(argv[3]) if len(argv) > 3 else 256 * MIB
        fs = open_fs(argv[2])
        walk(fs, fs.root, "", limit, sys.stdout)
        return 0
    if len(argv) >= 4 and argv[1] == "chunks":
        fs = open_fs(argv[2])
        fd = fs.get(argv[3])
        with fd.open() as stream:
            for text in argv[4:]:
                offset = int(text, 0)
                stream.seek(offset)
                print(f"{offset:#014x} {hashlib.sha256(stream.read(MIB)).hexdigest()}")
        return 0
    sys.stderr.write(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
