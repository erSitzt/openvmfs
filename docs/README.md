# OpenVmfs documentation

OpenVmfs reads VMware VMFS6 datastores, and the virtual disks on them, straight from a block device or an image
file. This is the map of what is written down about it.

## If you want to use it

| You want to | Read |
|---|---|
| Try it in five minutes | [guide/getting-started.md](guide/getting-started.md) |
| Look into a datastore from the shell | [guide/cli.md](guide/cli.md) |
| Use the library from Rust | [guide/rust.md](guide/rust.md) |
| Use it from C or any language with a C interface | [guide/c.md](guide/c.md) |
| Use it from .NET | [guide/dotnet.md](guide/dotnet.md) |
| Build a backup or recovery program on it | [guide/backup.md](guide/backup.md), then [consistency.md](consistency.md) |
| Understand an error or a refusal | [guide/errors.md](guide/errors.md) |
| Get a quick answer | [faq.md](faq.md) |

## If you want to understand VMFS

VMware publishes no specification of VMFS. Everything here was learned from existing open implementations and then
checked against volumes that ESXi wrote; every statement carries a mark that says how well it is confirmed.

| Topic | Read |
|---|---|
| Follow one read by hand, with a hex dump at every step | [internals/walkthrough.md](internals/walkthrough.md) |
| The words (SFB, LFB, sub-block, pointer block, TBZ, ...) | [internals/glossary.md](internals/glossary.md) |
| The on-disk format, structure by structure | [format/00-overview.md](format/00-overview.md) and the files next to it |
| Heartbeats and the metadata journal | [format/60-heartbeat-journal.md](format/60-heartbeat-journal.md) |
| Snapshot deltas (SESparse) | [format/80-sesparse.md](format/80-sesparse.md) |
| What is still unknown | [format/90-open-questions.md](format/90-open-questions.md) |
| Where the knowledge came from | [format/sources.md](format/sources.md) |
| What a storage snapshot of a datastore in use holds, measured | [consistency.md](consistency.md) |

## If you want to work on OpenVmfs

| Topic | Read |
|---|---|
| How the code is organised and why | [design.md](design.md) |
| How correctness is established, and how to run every check | [internals/testing.md](internals/testing.md) |
| The test images in the repository and what each one proves | [../fixtures/vmfs6/README.md](../fixtures/vmfs6/README.md) |
| Building a lab that produces ground truth: nested ESXi, capture, consistency experiments | [internals/lab.md](internals/lab.md) |
| The same lab as OpenTofu / Terraform configuration | [../lab/tofu/README.md](../lab/tofu/README.md) |
| Rules for contributions, in particular for using prior work | [../CONTRIBUTING.md](../CONTRIBUTING.md) |

The API reference is generated from the source: `cargo doc --no-deps --open`. The C interface is documented in
[`crates/openvmfs-capi/include/openvmfs.h`](../crates/openvmfs-capi/include/openvmfs.h).

## How to read the confidence marks

| Mark | Meaning |
|---|---|
| **[V]** | Verified: seen on a volume written by ESXi, and usually covered by a test on a fixture |
| **[2]** | Two independent implementations agree, not yet seen by us |
| **[1]** | One implementation says so |
| **[?]** | Disputed or unknown |

OpenVmfs reads only what is marked **[V]** and refuses the rest, unless it is told otherwise.
