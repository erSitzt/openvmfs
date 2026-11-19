# Sources

We read these to learn the format. No code was taken from any of them. "Family 1" and "family 2" are the two
independent lines of implementation that the confidence mark **[2]** refers to.

## Implementations

| Work | Licence | What we learned from it |
|---|---|---|
| [dissect.vmfs](https://github.com/fox-it/dissect.vmfs) (Fox-IT / NCC Group), **family 1** | AGPL-3.0-or-later | The most complete description of VMFS6: LVM header area and extent table, filesystem descriptor fields, where system file descriptors are found, resource file layout, 64-bit address layouts, file descriptor layout, addressing modes including double indirection, the VMFS6 directory format with its hash table. Also its sample images, used locally for tests. |
| [vmfs-tools](https://github.com/glandium/vmfs-tools) (C. Fillot, M. Hommey), **family 2** | GPL-2.0-or-later | The original reverse engineering of VMFS3 and VMFS5: the magic numbers, the metadata header with heartbeat fields, bitmap files. |
| [vmfs6-tools](https://github.com/teward/vmfs6-tools), [vmfs6-tool](https://github.com/weafon/vmfs6-tool) and the fork by [P. Herkert](https://github.com/philippherkert/vmfs6-tool), **family 2** | GPL-2.0-or-later | An independent reading of VMFS6: 8 KiB descriptors with 320 addresses, pointer blocks in the sub-block file, large file blocks read at the file offset modulo 512 MiB, bit 7 as the marker of never-written blocks, a zero address as a hole. Their issue trackers document the failure modes we test for: volumes above 16 TiB, thin and lazy-zeroed disks, the order in which system files are opened, SCSI reservations on open. |
| [fvmfs](https://code.google.com/archive/p/vmfs) (fluid Operations) | GPL-3.0 | History: the first open VMFS3 reader. |
| [QEMU `block/vmdk.c`](https://www.qemu.org) and the commit that added SESparse | MIT (this file) | The SESparse layout and the rule to dispatch on extent type. |
| [dissect.hypervisor](https://github.com/fox-it/dissect.hypervisor) | AGPL-3.0-or-later | A second reading of SESparse and COWD, and the disagreement about SESparse entries of type 1. |

## Documents

| Document | What we used |
|---|---|
| VMware, "Virtual Disk Format 5.0" technical note | Descriptor grammar; COWD header. |
| [libvmdk format documentation](https://github.com/libyal/libvmdk) (J. Metz, GFDL-1.3-or-later) | Descriptor keys seen in practice. |
| UEFI specification | GUID partition table. |
| Broadcom knowledge base articles on `vmkfstools -D` and `-t0`, snapshot LUN detection, VMFS partition tables, snapshot formats | How ESXi reports what a reader must reproduce; the partition type GUID. |
| E. Schamper, "I spent my summer reverse engineering ESXi VMFS", FOSDEM 2026 | The cause of wrong reads above 16 TiB; structure sizes. |
| C. Hogan, blog posts on VMFS6 small and large file blocks and sub-blocks (2017) | Block sizes and when ESXi uses which. |

## Tools used as black boxes

`dissect.vmfs`, `debugvmfs6` (vmfs6-tools) and `qemu-img` run in a container (`tools/oracle`) to compare results.
They are not linked into, vendored into or shipped with the OpenVmfs library, tools or packages. The comparison
script `tools/oracle/oracle.py` imports dissect.vmfs; it runs only in that container and is not part of any
package.
