# Contributing to OpenVmfs

## Prior work: read everything, copy nothing

OpenVmfs is Apache-2.0. The other VMFS and VMDK implementations are GPL or AGPL. We learn from them and take no
code from them.

1. **Read them.** `reference/` (git-ignored) holds clones of the prior implementations. Read how each of them
   handles the layer you work on, including their bug trackers and fix commits. Do not build or run them outside
   the oracle container.
2. **Write down what you learned** in `docs/format/`, in your own words: structures, algorithms, where the
   implementations differ. Every fact gets its source and a confidence mark (see `docs/format/00-overview.md`).
3. **Write your own code from that description.** Do not copy or translate files, functions, tables, comments or
   tests. A line-by-line port is copying. Our design differs on purpose: block maps are resolved once and are
   immutable, addresses are typed, and unverified layouts are refused.
4. **Let ESXi decide.** When implementations disagree, or only one of them knows a detail, build a fixture and
   check.
5. **Credit.** New sources go into `NOTICE` and `docs/format/sources.md`.

Reading copyleft code while writing a permissively licensed implementation is not a clean-room process. Format
facts and ideas are free to use; copied or closely translated code is not. Rule 3 is what keeps OpenVmfs
independent.

## Refuse instead of guessing

A backup that silently contains wrong data is worse than no backup. Therefore:

- Accept only values that a fixture written by ESXi has shown. Everything else is `ErrorKind::Unsupported` with a
  message that names the structure, the field, the value and the offset.
- Semantics known only from other implementations stay behind `OpenOptions::accept_unverified` until a fixture
  confirms them. Then remove the refusal and add the test.
- Library code never panics on any input. Use the checked helpers in `bytes.rs`; the workspace lints flag
  unchecked arithmetic, indexing and `unwrap`.
- The device is opened read-only. Never send SCSI reservations or any other command that changes state.

## Tests

- Parsers have unit tests on bytes built in the test.
- `crates/openvmfs-testkit/tests` reads real images. Expectations must be facts about the image (self-describing
  content, values confirmed by an independent reader), with a comment saying where they come from.
- Run `cargo fmt --all`, `cargo clippy --workspace --all-targets` and `cargo test --workspace` before a change.

## Fixtures

Fixtures are images of small datastores written by ESXi, with a manifest of what ESXi itself reported. Before a
fixture is committed, search the image for host names, addresses and other identifiers of the environment it came
from. Third-party sample images stay in `fixtures/external/` (git-ignored) and are never redistributed.

## Tests and documentation

How correctness is established, and how to run every check, is in
[`docs/internals/testing.md`](docs/internals/testing.md). A change in behaviour comes with its test and with the
documentation that describes it: the guides in `docs/guide/` for users, `docs/format/` for the format.
