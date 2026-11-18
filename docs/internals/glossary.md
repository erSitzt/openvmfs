# Glossary

The words of VMFS, as this project uses them. VMware's own names are used where they are known from ESXi's tools
(`vmkfstools`, `voma`) or from the names of the system files.

| Term | Meaning |
|---|---|
| **Address** | A 32- or 64-bit value that names a block, a sub-block, a pointer block or a file descriptor. The low three bits are its type. See [format/30](../format/30-addresses-resources.md) |
| **Addressing mode (ZLA)** | A field of a file descriptor that says what its address array holds: block addresses, sub-block addresses, pointer blocks, pointer blocks to pointer blocks, or nothing because the file is resident. `vmkfstools -D` prints it as `zla` |
| **Base disk** | The disk at the bottom of a snapshot chain: a descriptor and a flat file |
| **CBT, changed-block tracking** | A vSphere feature that records which areas of a disk changed (`-ctk.vmdk`). `version=3` in a descriptor says it is on. OpenVmfs does not read the tracking file |
| **CID, content id** | A number in a disk descriptor that changes when the disk's content is changed. A snapshot link records its parent's CID (`parentCID`); if they differ, the parent was modified behind the snapshot's back |
| **Cluster** | A group of resources in a resource file, with one header (lock block and bitmap). For small file blocks, a cluster is 512 blocks: exactly one large file block |
| **Cluster group** | A group of clusters in a resource file: first all their headers, then all their resources |
| **Datastore** | What vSphere calls a place for VM files. On block storage it is a VMFS volume |
| **Delta** | The file that holds what was written to a disk after a snapshot. On VMFS6 its format is SESparse |
| **Descriptor (disk)** | The small text file `name.vmdk` that describes a virtual disk and names its extent file |
| **Descriptor (file)**, FD | VMFS's inode: 8192 bytes with a lock block, the file's metadata and its address array. Lives in `.fdc.sf` |
| **Descriptor (filesystem)** | The structure at volume offset 0x200000 with the magic, version, UUID, label and block sizes |
| **Extent (LVM)** | A device that is part of a datastore; also a piece of a device in the volume's extent table |
| **Extent (VMDK)** | The file that holds a disk's data, named in the descriptor: `-flat.vmdk` or `-sesparse.vmdk` |
| **Flat file** | `name-flat.vmdk`: the data of a base disk, at the same offsets as the disk. Thin and thick disks differ only in how much of this file has blocks |
| **Grain** | The unit of a SESparse delta: 4 KiB |
| **Heartbeat** | A slot in a fixed region of the volume that a host refreshes while it holds locks. Other hosts use it to tell a live lock holder from a dead one |
| **Hole** | A part of a file without a block. Reads as zeros |
| **Journal** | Where a host writes a metadata change before it writes it to its place. One journal of 2 MiB per host, in `.jbc.sf`. See [format/60](../format/60-heartbeat-journal.md) |
| **LFB, large file block** | A block of 512 MiB. Thick disks on datastores over 128 GiB are made of these |
| **Lock block** | The first 4096 bytes of a file descriptor or cluster header: who holds it, and how |
| **Logical volume** | The address space that VMFS lives in, assembled by the LVM from one or more devices |
| **LVM** | The small volume manager under VMFS. Its header is 1 MiB into the partition |
| **PE, physical extent** | The LVM's unit of 256 MiB, and an entry in its table of pieces |
| **Pointer block, PB, PB2** | A block of 8192 addresses, used when a file has more blocks than its descriptor has room for. On VMFS6 pointer blocks are sub-blocks, or live in `.pb2.sf` |
| **RDM, raw device mapping** | A "disk" whose data is another LUN. The file on the datastore is only a pointer |
| **Resident** | A file so small that its content is inside its descriptor |
| **Resource file** | A system file that holds equally sized resources in clusters: `.fdc.sf` (file descriptors), `.sbc.sf` (sub-blocks), `.pb2.sf` (pointer blocks), `.fbb.sf` (the bitmap of file blocks), `.jbc.sf` (journals) |
| **SESparse** | "Space-efficient sparse": the format of snapshot deltas on VMFS6. See [format/80](../format/80-sesparse.md) |
| **SFB, small file block** | A block of 1 MiB, the normal allocation unit. Thin disks are made of these |
| **Snapshot (storage)** | A frozen copy of a LUN, made by the storage array. Knows nothing about VMFS |
| **Snapshot (VM)** | A point in time of a VM, made by vSphere: the disk is frozen and writes go to a new delta |
| **Snapshot chain** | A base disk and the deltas on top of it, linked by `parentFileNameHint` |
| **Spanned datastore** | A datastore that was extended onto further devices |
| **Sub-block, SB** | A block of 64 KiB in `.sbc.sf`, for small files and for pointer blocks |
| **System file** | A file of the filesystem itself, in the root directory, ending in `.sf` |
| **TBZ, to be zeroed** | A mark on an allocated block that was never written. It must read as zeros; what is on the device there is whatever was there before. Lazy-zeroed thick disks are full of them. `vmkfstools -t0` prints `Z` |
| **Thick, lazy zeroed** | A disk whose blocks are all allocated at creation and zeroed on first write |
| **Thick, eager zeroed** | A disk whose blocks are all allocated and zeroed at creation |
| **Thin** | A disk whose blocks are allocated when they are first written |
| **Volume offset** | An offset in the logical volume. Everything inside VMFS is addressed this way |
| **VMFS-L** | A variant of VMFS for ESXi's own boot devices. Not supported |
| **vmkfstools, voma** | ESXi's tools for virtual disks and for checking VMFS metadata; the source of ground truth in this project |
