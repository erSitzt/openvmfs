#!/usr/bin/env bash
# Compares what OpenVmfs reads from an image with what the independent reader in the oracle container reads.
# Every entry is compared by type, size and SHA-256 (files up to MAX_BYTES, default 256 MiB).
#
# Usage: tools/compare-with-oracle.sh IMAGE [MAX_BYTES]
# Needs: docker image `openvmfs-oracle` (docker build -t openvmfs-oracle tools/oracle) and a built CLI.
set -euo pipefail

image="$(realpath "$1")"
limit="${2:-268435456}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cli="${OPENVMFS_CLI:-$root/target/release/openvmfs}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

"$cli" manifest "$image" "$limit" > "$work/ours.tsv"
# The oracle wants the VMFS partition; tell it where that starts when the image is a whole disk.
offset="$("$cli" info "$image" | sed -n 's/^partition offset: *//p')"
docker run --rm -e "ORACLE_PARTITION_OFFSET=$offset" -v "$(dirname "$image"):/data:ro" openvmfs-oracle \
  manifest "/data/$(basename "$image")" "$limit" > "$work/oracle.tsv"

if diff -u "$work/oracle.tsv" "$work/ours.tsv" > "$work/diff"; then
  echo "identical: $(wc -l < "$work/ours.tsv") entries  $(basename "$image")"
else
  echo "DIFFERENT: $(basename "$image")"
  head -40 "$work/diff"
  exit 1
fi
