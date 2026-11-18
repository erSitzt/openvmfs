#!/usr/bin/env bash
# Builds the NuGet package of the .NET binding, with the native library for Linux x64 and Windows x64 inside
# (runtimes/<rid>/native), and leaves it in target/nuget/.
#
# Usage: tools/pack-nuget.sh [--skip-native]       (--skip-native packs whatever native libraries are already built)
# Needs: cargo, dotnet, and Docker for the Windows library (see tools/build-windows.sh).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ "${1:-}" != "--skip-native" ]; then
  (cd "$root" && cargo build --release -p openvmfs-capi)
  "$root/tools/build-windows.sh"
fi
for library in target/release/libopenvmfs_c.so target/x86_64-pc-windows-gnu/release/openvmfs_c.dll; do
  [ -f "$root/$library" ] || { echo "missing $library" >&2; exit 1; }
done

out="$root/target/nuget"
mkdir -p "$out"
dotnet pack "$root/bindings/dotnet/src/OpenVmfs" -c Release -o "$out" --nologo -v quiet
package="$(ls -t "$out"/OpenVmfs.*.nupkg | head -1)"
echo "$package"
# The package must carry both native libraries.
unzip -l "$package" | grep -E "runtimes/(linux-x64|win-x64)/native/" || { echo "native libraries are missing from the package" >&2; exit 1; }
