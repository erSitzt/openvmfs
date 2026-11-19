#!/usr/bin/env python3
"""Runs the Windows build's tests on a Windows machine, from Linux: files over SMB, commands over WinRM.

What it does:
  1. copies the test programs (tools/build-windows.sh --tests), the fixtures and, if it was published, the .NET test
     program into a new directory under the remote user's temp directory;
  2. runs every test program there and prints its result line;
  3. removes the directory again (unless --keep).

Nothing is installed and nothing outside that directory is touched.

Usage: tools/windows-remote-test.py --host HOST --env FILE [--user-key KEY] [--password-key KEY] [--keep]
                                    [--only NAME ...]
  FILE holds two lines, WINDOWS_USER=DOMAIN\\name and WINDOWS_PASSWORD=...  (mode 600; never printed).
  --user-key and --password-key name other keys, to reuse a credentials file that already exists.
Needs: a Python with `pywinrm` and `smbprotocol`; WinRM (5985, NTLM) and SMB (445) open on the Windows side.
       Run it with `python -I`, so that nothing in the current directory shadows a standard module.
.NET tests: dotnet publish bindings/dotnet/tests/OpenVmfs.Tests -c Release -r win-x64 --self-contained -o target/dotnet-win
"""
import argparse
import io
import pathlib
import re
import sys
import tarfile
import time

import smbclient
import winrm

ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD = ROOT / "target/x86_64-pc-windows-gnu/release"
# The unpacked images hold about 6 GiB of data (they are sparse); stop if they use much more than that.
MAX_DISK_USE = 12 << 30


def load_env(path, user_key, password_key):
    values = {}
    for line in pathlib.Path(path).expanduser().read_text().splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, value = line.split("=", 1)
            values[key.strip()] = value.strip()
    user, password = values.get(user_key), values.get(password_key)
    if not user or not password:
        sys.exit(f"{path}: {user_key} and {password_key} are needed")
    return user, password


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--host", required=True)
    parser.add_argument("--env", required=True)
    parser.add_argument("--user-key", default="WINDOWS_USER", help="key of the user name in the env file")
    parser.add_argument("--password-key", default="WINDOWS_PASSWORD", help="key of the password in the env file")
    parser.add_argument("--keep", action="store_true", help="leave the files on the Windows machine")
    parser.add_argument("--only", nargs="*", default=[], help="run only test programs whose name contains one of these")
    args = parser.parse_args()
    user, password = load_env(args.env, args.user_key, args.password_key)

    session = winrm.Session(f"http://{args.host}:5985/wsman", auth=(user, password), transport="ntlm",
                            message_encryption="always", operation_timeout_sec=120, read_timeout_sec=150)
    smbclient.register_session(args.host, username=user, password=password)

    def ps(script):
        result = session.run_ps(script)
        return result.status_code, result.std_out.decode("utf-8", "replace"), result.std_err.decode("utf-8", "replace")

    def unc(path):
        return f"\\\\{args.host}\\{path[0]}$\\{path[3:]}"

    code, out, err = ps("$o = Get-CimInstance Win32_OperatingSystem; $t = [IO.Path]::GetTempPath().TrimEnd('\\'); "
                        "$d = Get-PSDrive $t.Substring(0,1); \"$($o.Caption) $($o.Version)|$t|$($d.Free)\"")
    if code:
        sys.exit(f"cannot run commands on {args.host}: {err[-400:]}")
    system, temp, free = out.strip().split("|")
    free = int(free)
    print(f"{system}; {free >> 30} GiB free on {temp[:2]}")
    if free < 3 * MAX_DISK_USE:
        sys.exit("too little free space; not copying anything")
    base = f"{temp}\\openvmfs-test-{int(time.time())}"

    # One tar with everything, unpacked there with the tar.exe that Windows ships.
    # Test programs carry a hash in their name; the tools next to them (openvmfs.exe, trace_reads.exe) do not.
    programs = sorted(p for p in BUILD.glob("build/*/*/out/*.exe") if re.search(r"-[0-9a-f]{16}\.exe$", p.name))
    if not programs:
        sys.exit("no test programs; run tools/build-windows.sh --tests first")
    package = io.BytesIO()
    with tarfile.open(fileobj=package, mode="w") as tar:
        for program in [*programs, BUILD / "openvmfs.exe", BUILD / "openvmfs_c.dll"]:
            tar.add(program, f"bin/{program.name}")
        tar.add(ROOT / "fixtures/vmfs6", "repo/fixtures/vmfs6")
        external = ROOT / "fixtures/external"
        if external.is_dir():
            tar.add(external, "repo/fixtures/external")
        dotnet = ROOT / "target/dotnet-win"
        if (dotnet / "OpenVmfs.Tests.exe").is_file():
            tar.add(dotnet, "repo/dotnet", filter=lambda item: None if item.name.endswith((".pdb", ".so")) else item)
    print(f"copying {package.tell() >> 20} MiB to {base}")
    smbclient.makedirs(unc(base), exist_ok=True)
    package.seek(0)
    with smbclient.open_file(unc(f"{base}\\package.tar"), mode="wb") as target:
        while chunk := package.read(4 << 20):
            target.write(chunk)
    code, out, err = ps(f"Set-Location '{base}'; tar.exe -xf package.tar; Remove-Item package.tar; (Get-ChildItem bin).Count")
    if code:
        sys.exit(f"unpacking failed: {out} {err[-400:]}")

    failures = 0
    try:
        environment = ("$ProgressPreference = 'SilentlyContinue'; "
                       f"$env:OPENVMFS_REPO_ROOT = '{base}\\repo'; "
                       f"$env:OPENVMFS_FIXTURE_CACHE = '{base}\\repo\\target\\fixture-cache'; ")
        names = [p.name for p in programs if not args.only or any(word in p.name for word in args.only)]
        # The .NET tests read images that the Rust tests unpack, so they run last.
        runs = [(name, f"& '{base}\\bin\\{name}' 2>&1 | Out-String -Width 300") for name in names]
        if (ROOT / "target/dotnet-win/OpenVmfs.Tests.exe").is_file() and not args.only:
            runs.append(("OpenVmfs.Tests.exe (.NET)", f"& '{base}\\repo\\dotnet\\OpenVmfs.Tests.exe' 2>&1 | Out-String -Width 300"))
        for name, command in runs:
            started = time.time()
            code, out, err = ps(environment + command + "; \"EXIT=$LASTEXITCODE\"")
            exit_code = (re.findall(r"EXIT=(-?\d*)", out) or ["?"])[-1]
            # Rust prints "test result: ok. 5 passed; 0 failed", xUnit "Total: 7, Errors: 0, Failed: 0, Skipped: 0".
            results = re.findall(r"test result: \w+\. \d+ passed; (\d+) failed|Total: \d+, Errors: (\d+), Failed: (\d+)", out)
            summary = [line.strip() for line in out.splitlines()
                       if re.search(r"test result:|Total: \d+|FAILED|panicked|blocked|Gruppenrichtlinie", line)]
            ok = exit_code == "0" and bool(results) and all(not any(int(n) for n in found if n) for found in results)
            failures += not ok
            print(f"{'ok  ' if ok else 'FAIL'} {name} ({time.time() - started:.0f} s): {' | '.join(summary)[:400] or (out + err)[-400:].strip()}",
                  flush=True)
            _, used, _ = ps(f"$d = Get-PSDrive '{temp[0]}'; $d.Free")
            if used.strip().isdigit() and free - int(used.strip()) > MAX_DISK_USE:
                print("stopping: the test files use more disk space than expected (images are not sparse?)")
                failures += 1
                break
        code, out, _ = ps(f"& '{base}\\bin\\openvmfs.exe' info '{base}\\repo\\target\\fixture-cache\\ours\\fixture-zero-v1.fixture.img' 2>&1 | Out-String")
        print("openvmfs.exe info:", " / ".join(line.strip() for line in out.splitlines()[:4]))
    finally:
        if args.keep:
            print(f"left in place: {base}")
        else:
            code, _, err = ps(f"Remove-Item -Recurse -Force '{base}'; Test-Path '{base}'")
            print("removed the test directory" if code == 0 else f"could not remove {base}: {err[-200:]}")
    print(f"{failures} test program(s) failed" if failures else "all test programs passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
