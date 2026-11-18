#!/usr/bin/env bash
# Downloads third-party VMFS sample images for local development into fixtures/external/.
#
# The images come from the test data of fox-it/dissect.vmfs (AGPL-3.0, stored in Git LFS). They are used locally
# only: the directory is git-ignored and nothing from it is committed or redistributed. Tests that need them skip
# when they are absent.
#
# Usage: fixtures/fetch-external.sh [name ...]      (default: the VMFS6 and LVM6 images)
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
dest="$here/external/dissect.vmfs"
base="https://media.githubusercontent.com/media/fox-it/dissect.vmfs/main/tests/_data"

default=(
  vmfs/vmfs6.bin.gz
  vmfs/vmfs6d.bin.gz
  vmfs/vmfs6l.bin.gz
  vmfs/vmfs6ld.bin.gz
  vmfs/vmfs6-resource-optimized.bin.gz
  vmfs/zero6.bin.gz
  vmfs/span/vmfs6-span-0.bin.gz
  vmfs/span/vmfs6-span-1.bin.gz
  lvm/lvm6.bin.gz
  lvm/span/lvm6-span-0.bin.gz
  lvm/span/lvm6-span-1.bin.gz
)

if [ "$#" -gt 0 ]; then names=("$@"); else names=("${default[@]}"); fi

for name in "${names[@]}"; do
  out="$dest/$name"
  if [ -s "$out" ]; then
    echo "have  $name"
    continue
  fi
  mkdir -p "$(dirname "$out")"
  echo "fetch $name"
  curl --fail --silent --show-error --location --output "$out.part" "$base/$name"
  mv "$out.part" "$out"
done

ls -l "$dest"/*/ 2>/dev/null || true
