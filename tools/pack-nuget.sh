#!/usr/bin/env bash
# Builds the NuGet package of the .NET binding, with the native library for Linux x64 and Windows x64 inside
# (runtimes/<rid>/native), and leaves it in target/nuget/.
#
# Usage: tools/pack-nuget.sh [--skip-native]       (--skip-native packs whatever native libraries are already built)
# Needs: cargo, dotnet, and Docker for the Windows library (see tools/build-windows.sh).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The package is built apart from everyday builds, with the paths of this machine mapped away: a library that goes
# to a public feed must not carry the builder's home directory.
export CARGO_TARGET_DIR=target/package
native="$root/$CARGO_TARGET_DIR"
if [ "${1:-}" != "--skip-native" ]; then
  (cd "$root" && RUSTFLAGS="--remap-path-prefix=$root=. --remap-path-prefix=$HOME/.cargo=cargo --remap-path-prefix=$HOME/.rustup=rustup" \
    cargo build --release -p openvmfs-capi)
  OPENVMFS_WINDOWS_RUSTFLAGS="--remap-path-prefix=/work=." "$root/tools/build-windows.sh"
fi
for library in release/libopenvmfs_c.so x86_64-pc-windows-gnu/release/openvmfs_c.dll; do
  [ -f "$native/$library" ] || { echo "missing $native/$library" >&2; exit 1; }
done

out="$root/target/nuget"
mkdir -p "$out"
rm -rf "$root/bindings/dotnet/src/OpenVmfs/obj" "$root/bindings/dotnet/src/OpenVmfs/bin"
dotnet pack "$root/bindings/dotnet/src/OpenVmfs" -c Release -o "$out" --nologo -v quiet \
  -p:NativeRoot="$native/" -p:ContinuousIntegrationBuild=true -p:Deterministic=true -p:PathMap="$root=/_"
package="$(ls -t "$out"/OpenVmfs.*.nupkg | head -1)"
echo "$package"
unzip -l "$package" | grep -E "runtimes/(linux-x64|win-x64)/native/" || { echo "native libraries are missing from the package" >&2; exit 1; }

# Nothing in the package may name this machine or its user.
check="$(mktemp -d)"
trap 'rm -rf "$check"' EXIT
unzip -q "$package" -d "$check"
# (The user name is looked for as a path component: on a build service it is an everyday word.)
if grep -r -a -l -F -e "$HOME" -e "$root" -e "/$(id -un)/" -e "\\$(id -un)\\" "$check"; then
  echo "the files above contain a path or user name of this machine" >&2
  exit 1
fi
echo "no local paths in the package"
