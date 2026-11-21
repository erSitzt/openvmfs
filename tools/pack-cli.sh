#!/usr/bin/env bash
# Builds the command line tool for Linux x64 (static, runs on any distribution) and Windows x64 and leaves two
# archives and their SHA-256 sums in target/cli/.
#
# Usage: tools/pack-cli.sh
# Needs: cargo with the target x86_64-unknown-linux-musl (rustup target add ...), Docker for the Windows program
# (see tools/build-windows.sh), zip.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -1)"
# Built apart from everyday builds, with the paths of this machine mapped away (as tools/pack-nuget.sh does).
export CARGO_TARGET_DIR=target/package
RUSTFLAGS="--remap-path-prefix=$root=. --remap-path-prefix=$HOME/.cargo=cargo --remap-path-prefix=$HOME/.rustup=rustup" \
  cargo build --release --target x86_64-unknown-linux-musl -p openvmfs-cli
OPENVMFS_WINDOWS_RUSTFLAGS="--remap-path-prefix=/work=." tools/build-windows.sh

out="$root/target/cli"
rm -rf "$out"
mkdir -p "$out"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
for platform in linux-x64 windows-x64; do
  name="openvmfs-$version-$platform"
  mkdir "$stage/$name"
  cp LICENSE NOTICE crates/openvmfs-cli/THIRD-PARTY-NOTICES.txt "$stage/$name/"
  if [ "$platform" = linux-x64 ]; then
    cp "$CARGO_TARGET_DIR/x86_64-unknown-linux-musl/release/openvmfs" "$stage/$name/"
    tar -C "$stage" --owner=0 --group=0 --numeric-owner -czf "$out/$name.tar.gz" "$name"
  else
    cp "$CARGO_TARGET_DIR/x86_64-pc-windows-gnu/release/openvmfs.exe" "$stage/$name/"
    (cd "$stage" && zip -q -r "$out/$name.zip" "$name")
  fi
done

# Nothing in the programs may name this machine or its user.
if grep -r -a -l -F -e "$HOME" -e "$root" -e "/$(id -un)/" -e "\\$(id -un)\\" "$stage"; then
  echo "the files above contain a path or user name of this machine" >&2
  exit 1
fi
(cd "$out" && sha256sum openvmfs-* > SHA256SUMS && cat SHA256SUMS)
"$stage/openvmfs-$version-linux-x64/openvmfs" 2>&1 | head -1 || true
