# How correctness is established

A reader of an undocumented filesystem can be wrong in ways that no unit test notices: it can agree with its
author's misunderstanding. So the tests of OpenVmfs lean on things that do not come from OpenVmfs.

## Where the truth comes from

| Source | What it proves | Used for |
|---|---|---|
| **ESXi itself.** Hashes computed on the host (`sha256sum`), block maps (`vmkfstools -t0`), metadata (`vmkfstools -D`, `-Ph`), consolidated clones (`vmkfstools -i`), and `voma` for the health of the volume | That OpenVmfs returns what ESXi returns | Every fixture in `fixtures/vmfs6` |
| **Self-describing test data.** Each written sector carries a magic, the number of its disk, its own sector number and a checksum (`fixtures/recipes/pattern.py`) | That a sector is returned at the right place of the right disk, without trusting any reader | All test disks |
| **Junk under everything.** Before the test disks were created, the datastore was filled with recognisable junk and emptied again, with automatic unmap switched off | That blocks which must read as zeros do so, although the device holds something else there | `fixture-zero-v1`, `fixture-lfb-v1` |
| **Independent readers.** dissect.vmfs for the filesystem, QEMU for snapshot deltas, run in a container | A second opinion that shares no code with OpenVmfs | `tools/compare-with-oracle.sh`, the public sample images |
| **The order of writes.** A guest writes in a known order and waits for each write | That an image of a datastore in use is a state a crash could have left | [`../consistency.md`](../consistency.md) |

The fixtures and what each one proves are listed in
[`fixtures/vmfs6/README.md`](../../fixtures/vmfs6/README.md).

## Running the checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets          # must print no warnings
cargo test --workspace --release                 # release: the fixture tests hash gigabytes
cargo build --release -p openvmfs-capi
dotnet test --solution bindings/dotnet/OpenVmfs.slnx -c Release
```

The tests that need the public sample images of dissect.vmfs skip themselves when the images are missing. Fetch
them once with `fixtures/fetch-external.sh`; they are AGPL-licensed test data of another project and are not
redistributed here.

Fixtures are unpacked on first use into `target/fixture-cache` (about 6 GiB, sparse). Set
`OPENVMFS_FIXTURE_CACHE` to put that elsewhere.

### What the suites are

| Suite | What it does |
|---|---|
| `crates/openvmfs` unit tests | Every parser against structures built byte by byte, including each refusal |
| `sample_images` | The public sample images: listing, small files, disks, and a comparison of block maps |
| `fixture_zero`, `fixture_lfb`, `fixture_guest` | Base disks: thin, lazy-zeroed, eager-zeroed, large file blocks, disks written by a guest |
| `fixture_chain`, `fixture_chain2` | Snapshot chains of one to three levels, with guest discards, against ESXi's consolidation |
| `fixture_big`, `fixture_span` | A 17 TiB datastore; a datastore on two devices |
| `fixture_inuse`, `fixture_journal` | Images of a datastore in use: lock detection, journal replay |
| `direct_io` | Unbuffered reads against buffered reads, for arbitrary offsets and lengths |
| `robustness` | Thousands of runs with damaged metadata: no panic, no read outside the device |
| `c_abi` | A C program and the C example, compiled against the header and run |
| `OpenVmfs.Tests` | The same facts through the .NET binding |

### Comparing with another reader

```bash
docker build -t openvmfs-oracle tools/oracle
tools/compare-with-oracle.sh IMAGE[,IMAGE...]
```

prints `identical: N entries` when OpenVmfs and dissect.vmfs agree on every file's type, size and SHA-256.

### Windows

The Windows build is cross-compiled on Linux and tested on a Windows machine:

```bash
tools/build-windows.sh --tests
dotnet publish bindings/dotnet/tests/OpenVmfs.Tests -c Release -r win-x64 --self-contained -o target/dotnet-win
python -I tools/windows-remote-test.py --host HOST --env FILE
```

The last command copies the test programs and fixtures into a temporary directory on the Windows machine (SMB),
runs them (WinRM), prints one line per program and removes the directory again.

## Partial images

A 17 TiB device cannot be stored, and need not be: a test reads a few megabytes of it. Large devices are captured
only where a reader looks:

1. `fixtures/recipes/capture_partial.py` fetches the head of the device and the ranges ESXi lists for the test
   files.
2. `cargo run --release -p openvmfs-testkit --example trace_reads -- IMAGE PATH...` prints every range OpenVmfs
   reads; `fixtures/recipes/fetch_missing.py` fetches what is not there yet. Repeat until nothing is missing.
3. `fixtures/recipes/pack_partial.py` packs the ranges and blanks the lab's addresses.

In the tests such an image answers reads **outside** the captured ranges with an error, not with zeros. A test can
therefore never pass because the reader looked at a place that was simply not captured.

## Rules for new tests

- A new format fact needs a fixture written by ESXi, with ESXi's own view of it stored next to it (`truth/`).
- A structure that has not been seen on such a fixture is refused, and there is a unit test for the refusal.
- Fixtures are never overwritten. A changed fixture gets a new version number in its name.
- Before a fixture is committed, it is searched for host names and addresses of the lab it came from.
