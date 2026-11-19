# Questions and answers

### Is it safe to point OpenVmfs at a production LUN?

It opens the device read-only and issues nothing but reads: no writes, no SCSI reservations, no locks. Reading the
live LUN of a datastore in use is still a bad idea, because the content changes while you read; read a storage
snapshot. And never present a copy of a datastore to an ESXi host that has the original mounted.

### Can I read the disks of running VMs?

From a storage snapshot, yes, if a VM snapshot was taken first: you then read the disk below that VM snapshot.
Without a VM snapshot, the disk on the storage snapshot lacks its newest blocks, and OpenVmfs refuses it. See
[guide/backup.md](guide/backup.md) and [consistency.md](consistency.md).

### Which versions are supported?

VMFS 6.82, which is what both ESXi 7.0.3 and ESXi 8.0 U3 format. Our own fixtures come from those two versions;
the one difference found between them is a header field of snapshot deltas
([format/80-sesparse.md](format/80-sesparse.md)). The public sample images that the tests also read were made by
another installation, whose version we do not know. VMFS5, VMFS-L and VMFS 6.81 are refused: there is no verified
sample.

### What about vSAN, vVols and NFS datastores?

None of them is VMFS. On NFS a datastore is a directory of ordinary files, so nothing needs to be decoded at the
filesystem level; vSAN and vVols store disks as objects in their own formats. OpenVmfs does not read them.

### Can it write, or restore to a datastore?

No, and it will not. Writing to a filesystem whose rules are known only from observation would risk other people's
data.

### Why does it refuse so much?

Because the older free tools show what guessing does: they return wrong data without an error on thin and
lazy-zeroed disks, on volumes over 16 TiB and on large files. A backup made that way is worse than none. OpenVmfs
reads what was verified against ESXi and says `Unsupported` otherwise, so that the caller can read the disk another
way. [guide/errors.md](guide/errors.md) lists the cases.

### Something is refused that ESXi reads fine. What now?

That is expected for layouts nobody has given us a sample of. Open an issue with the message, the output of
`openvmfs info` and the ESXi version. If you can reproduce the layout on a small test datastore
([internals/lab.md](internals/lab.md)), that sample is what turns a refusal into support.

### Does it detect corruption?

Only contradictions: wrong signatures, offsets outside the volume, counts that do not match. VMFS metadata has no
checksums that we know of, so a flipped bit in a block address goes unnoticed. Use `voma` on ESXi to check a
datastore.

### How fast is it?

Not measured yet. By design a read is a lookup in memory plus one read from the device, without locks, so it
should be as fast as the device and scale with threads.

### How does it relate to vmfs-tools, vmfs6-tools and dissect.vmfs?

They came first, and OpenVmfs learned the format from reading them; [format/sources.md](format/sources.md) says
what came from where. No code was taken: they are under GPL and AGPL, OpenVmfs is under Apache-2.0 so that it can
be embedded anywhere. The differences in behaviour are what the tests are about: lazy-zeroed blocks, large
volumes, snapshot chains, datastores in use.

### Is the format description reliable?

Each statement in [`format/`](format/00-overview.md) says how it is known: verified on a volume written by ESXi,
agreed on by two implementations, or claimed by one. What is still unknown is collected in
[format/90-open-questions.md](format/90-open-questions.md). It is a description of what was observed, not a
specification.

### Can I use it in a commercial product?

Yes. The licence is Apache-2.0.
