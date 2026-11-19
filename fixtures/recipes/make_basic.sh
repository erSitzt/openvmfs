#!/bin/sh
# Builds the "basic" test datastore on an ESXi host and records ESXi's own view of it. Runs on the ESXi shell.
#
#   make_basic.sh DEVICE LABEL OUT_DIR
#     DEVICE   a whole test disk, for example /vmfs/devices/disks/naa.6000...   (it is repartitioned!)
#     LABEL    name of the datastore to create
#     OUT_DIR  where the truth files go (sha256.txt, *.t0.txt, *.D.txt, ls.txt, version.txt)
#
# Needs pattern.py next to this script. What it creates:
#   pat/thin    1 GiB thin, written at four places           pat/lazy   300 MiB lazy-zeroed, three places
#   pat/eager   64 MiB eager-zeroed, fully written           pat/big    1100 MiB thin, written far apart
#   files/      small files: resident, sub-block, one over 1 MiB, a symlink, a directory with 300 files
# Before that the datastore is filled with junk and emptied, with automatic unmap off, so that every block a reader
# must not return holds recognisable bytes.
set -e
D="$1"; L="$2"; OUT="$3"
HERE="$(cd "$(dirname "$0")" && pwd)"
P="python $HERE/pattern.py"
V="/vmfs/volumes/$L"

partedUtil mklabel "$D" gpt
END=$(partedUtil getUsableSectors "$D" | cut -d" " -f2)
partedUtil setptbl "$D" gpt "1 2048 $END AA31E02A400F11DB9590000C2911D1B8 0" >/dev/null
vmkfstools -C vmfs6 -S "$L" "$D:1" >/dev/null
esxcli storage vmfs reclaim config set --volume-label="$L" --reclaim-priority=none

# Junk under everything.
FREE=$(df -m "$V" | tail -1 | awk '{print $4}')
$P junk "$V/junk.bin" $((FREE - 64)) >/dev/null
rm "$V/junk.bin"

mkdir "$V/pat" "$V/files" "$V/files/many"
vmkfstools -c 1g -d thin "$V/pat/thin.vmdk" >/dev/null
vmkfstools -c 300m -d zeroedthick "$V/pat/lazy.vmdk" >/dev/null
vmkfstools -c 64m -d eagerzeroedthick "$V/pat/eager.vmdk" >/dev/null
vmkfstools -c 1100m -d thin "$V/pat/big.vmdk" >/dev/null
$P write "$V/pat/thin-flat.vmdk" 1 1 0 2048 10240 2048 614400 4096 2095104 2048
$P write "$V/pat/lazy-flat.vmdk" 2 1 0 8 204800 2048 614392 8
$P write "$V/pat/eager-flat.vmdk" 3 1 0 131072
$P write "$V/pat/big-flat.vmdk" 4 1 0 2048 1048576 2048 2250752 2048

printf 'tiny\n' > "$V/files/tiny.txt"
dd if=/dev/urandom of="$V/files/subblock.bin" bs=1024 count=40 2>/dev/null
dd if=/dev/urandom of="$V/files/blocks.bin" bs=1024 count=3072 2>/dev/null
ln -s tiny.txt "$V/files/link"
i=0
while [ $i -lt 300 ]; do
  printf 'file %d\n' $i > "$V/files/many/$(printf 'f%04d' $i)"
  i=$((i + 1))
done

mkdir -p "$OUT"
vmware -vl > "$OUT/version.txt"
vmkfstools -Ph -v10 "$V" >> "$OUT/version.txt"
cd "$V"
sha256sum pat/*-flat.vmdk files/tiny.txt files/subblock.bin files/blocks.bin files/many/f0000 files/many/f0299 > "$OUT/sha256.txt"
for n in thin lazy eager big; do
  vmkfstools -t0 "pat/$n.vmdk" > "$OUT/$n.t0.txt" 2>&1
  vmkfstools -D "pat/$n-flat.vmdk" > "$OUT/$n-flat.D.txt" 2>&1
done
ls -la pat files > "$OUT/ls.txt"
ls files/many | wc -l >> "$OUT/ls.txt"
cd /
echo "built $L on $D"
