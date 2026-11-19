#!/usr/bin/env bash
# Builds openvmfs.exe and openvmfs_c.dll for Windows x64 in a container and leaves them in
# target/x86_64-pc-windows-gnu/release/.
#
# Usage: tools/build-windows.sh [--tests]
# Set OPENVMFS_RUST_IMAGE to use a mirror of the rust:1-bookworm image.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
toolchain="$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/rust-toolchain.toml")"
base="${OPENVMFS_RUST_IMAGE:-rust:1-bookworm}"

docker build -q -t openvmfs-winbuild --build-arg "BASE=$base" --build-arg "TOOLCHAIN=$toolchain" "$root/tools/windows" >/dev/null
run() {
  docker run --rm --user "$(id -u):$(id -g)" -v "$root:/work" -w /work \
    -e CARGO_HOME=/work/target/windows-cargo-home -e HOME=/tmp \
    -e CARGO_TARGET_DIR -e RUSTFLAGS="${OPENVMFS_WINDOWS_RUSTFLAGS:-}" \
    openvmfs-winbuild "$@"
}
# CARGO_TARGET_DIR, if set, must be relative to the repository (it is used inside the container too).
out="$root/${CARGO_TARGET_DIR:-target}/x86_64-pc-windows-gnu/release"
run cargo build --release --target x86_64-pc-windows-gnu -p openvmfs-cli -p openvmfs-capi
ls -l "$out/openvmfs.exe" "$out/openvmfs_c.dll"

# --tests also builds the test programs, to be run on a Windows machine (tools/windows-remote-test.py).
if [ "${1:-}" = "--tests" ]; then
  find "$out/build" -path '*/out/*.exe' -delete 2>/dev/null || true
  run cargo test --release --no-run --target x86_64-pc-windows-gnu --workspace
  find "$out/build" -path '*/out/*.exe' | wc -l
fi
