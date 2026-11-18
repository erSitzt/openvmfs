#!/usr/bin/env bash
# Builds openvmfs.exe and openvmfs_c.dll for Windows x64 in a container and leaves them in
# target/x86_64-pc-windows-gnu/release/.
#
# Usage: tools/build-windows.sh
# Set OPENVMFS_RUST_IMAGE to use a mirror of the rust:1-bookworm image.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/rust-toolchain.toml")"
base="${OPENVMFS_RUST_IMAGE:-rust:1-bookworm}"

docker build -q -t openvmfs-winbuild --build-arg "BASE=$base" --build-arg "TOOLCHAIN=$toolchain" "$root/tools/windows" >/dev/null
docker run --rm --user "$(id -u):$(id -g)" -v "$root:/work" -w /work \
  -e CARGO_HOME=/work/target/windows-cargo-home -e HOME=/tmp \
  openvmfs-winbuild \
  cargo build --release --target x86_64-pc-windows-gnu -p openvmfs-cli -p openvmfs-capi
ls -l "$root/target/x86_64-pc-windows-gnu/release/openvmfs.exe" "$root/target/x86_64-pc-windows-gnu/release/openvmfs_c.dll"
