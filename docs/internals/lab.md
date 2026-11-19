# Building a lab that produces ground truth

Everything OpenVmfs knows was checked against volumes that ESXi wrote. This page describes the lab that produces
such volumes, so that anyone can repeat an experiment or add a new one. It needs a vSphere environment where you
may create one VM, and nothing else.

The VM, its test disks, the datastores and the guests can also be created from the OpenTofu configuration in
[`lab/tofu`](../../lab/tofu/README.md).

Commands are given with [`govc`](https://github.com/vmware/govmomi/tree/main/govc) and the ESXi shell. Names are
examples.

## The setup

```
your workstation ──ssh/govc──► nested ESXi (a VM, "outer VM")
                                 ├── small virtual disks, each formatted as a VMFS6 datastore
                                 └── tiny guest VMs that write test data
```

- **A nested ESXi host**: a VM with hardware virtualisation exposed to the guest, 4 CPUs, 8 GiB of memory, a small
  boot disk, installed from the ESXi ISO. Evaluation mode is enough.
- **Test datastores**: further thin disks of the outer VM. Small ones (3 to 16 GiB) can be downloaded whole;
  large ones (256 GiB, 17 TiB) cost almost nothing as long as they stay empty.
- **Guests**: a small Linux live ISO (Alpine) in a VM on the nested host, with the test disks attached.

Enable SSH on the nested host and install a key. ESXi's SSH server did not accept ed25519 keys in our case; ECDSA
worked.

## Making a datastore

On the ESXi shell (replace the device name; `ls /vmfs/devices/disks` lists them):

```sh
D=/vmfs/devices/disks/mpx.vmhba0:C0:T1:L0
partedUtil mklabel $D gpt
END=$(partedUtil getUsableSectors $D | cut -d" " -f2)
partedUtil setptbl $D gpt "1 2048 $END AA31E02A400F11DB9590000C2911D1B8 0"
vmkfstools -C vmfs6 -S fz $D:1
esxcli storage vmfs reclaim config set --volume-label=fz --reclaim-priority=none
```

The last line switches automatic unmap off, so that stale data stays where it is. That matters for the next step.

A second device is added to a datastore with `vmkfstools -Z $D2:1 $D:1`.

## Junk first

Fill the fresh datastore with junk and delete it again, before creating test disks
(`fixtures/recipes/pattern.py junkfill`). Every block that a reader must not return then holds recognisable bytes
(`OVMFSJNK`), and a reader that returns stale data is caught by its content.

## Test disks

```sh
vmkfstools -c 1g -d thin             pat/thin.vmdk
vmkfstools -c 300m -d zeroedthick    pat/lazy.vmdk
vmkfstools -c 64m -d eagerzeroedthick pat/eager.vmdk
python pattern.py write pat/thin-flat.vmdk 1 1  0 2048  10240 2048     # disk id, generation, then (sector, count)...
```

`pattern.py write` writes self-describing sectors at the given places, straight into the flat file from the ESXi
shell. Writing through a guest gives the same metadata; a guest is needed for snapshots, discards and
write-order experiments.

Placement tricks:

- To get blocks **above 16 TiB**, fill the datastore with lazy-zeroed disks first (`vmkfstools -c 4100g -d
  zeroedthick`): they take all the low blocks and cost only their pointer blocks.
- To get **large file blocks**, the datastore must be larger than 128 GiB.
- To get files on the **second device** of a spanned datastore, fill the first.

## Recording what ESXi sees

For every fixture, store ESXi's own view next to the image (`truth/`):

```sh
sha256sum pat/thin-flat.vmdk                    # the content
vmkfstools -t0 pat/thin.vmdk                    # the block map, with device offsets
vmkfstools -D pat/thin-flat.vmdk                # the file descriptor
vmkfstools -Ph -v10 /vmfs/volumes/fz            # the volume
vmkfstools -i guest/disk-000002.vmdk -d thin clone/level2.vmdk    # a snapshot level, consolidated by ESXi
esxcli storage filesystem unmount -l fz && voma -m vmfs -f check -d $D:1
```

## Capturing the device

Unmount the datastore, read the device, mount it again:

```sh
esxcli storage filesystem unmount -l fz
dd if=$D bs=1M        # over ssh, into a sparse file on the workstation
esxcli storage filesystem mount -l fz
```

Large devices are captured in part; see [testing.md](testing.md).

## A storage snapshot without a storage array

To study what a crash-consistent image of a datastore **in use** holds, the outer VM plays the array:

1. take a snapshot of the outer VM, without memory. Its disks are frozen at that instant;
2. copy the frozen base disk of the test datastore (`govc datastore.cp`) and remove the snapshot;
3. download the copy.

The copy is what a storage snapshot of the LUN would be, and no ESXi host has seen it, so nothing has replayed its
journal. `fixtures/recipes/consistency_probe.py` and `consistency_marker.py` automate this together with the guest
workload and the comparison.

## Driving a guest without a network

The guests need no network and no agent: `govc vm.keystrokes -vm NAME -s 'command'` types into the console, and
`govc vm.console -capture file.png NAME` takes a picture of it.

## Hazards

- **Never attach a copy of a datastore to an ESXi host that has the original mounted.** Virtual disks without
  unique device ids look the same to ESXi. In our lab the host switched the volume to the copy and unmounted the
  datastore when the copy was removed; a rescan brought it back. Download copies instead.
- **A key sent while a snapshot is being taken is lost.** Send Enter first, then take the snapshot.
- **Do not change the outer VM's hardware while it has a snapshot** that an experiment still needs.
- **A thin 17 TiB disk is an overcommitment.** Nothing writes to it except VMFS metadata (a few hundred MiB), but
  delete it as soon as its parts are captured.
- **Images carry the lab's identity.** The heartbeat region holds the host's IP address as text, and file
  descriptors hold host UUIDs. `pack_partial.py --blank` removes the former; decide about the latter.
- **Sparse files of 17 TiB** do not fit on ext4 (16 TiB per file). A memory-backed filesystem (`/dev/shm`) holds
  them, since only the captured megabytes use memory.

## Things that were tried and are worth knowing

- The ESXi shell's Python is enough for the recipes; `tr` and `paste` are missing from its busybox.
- `vmkfstools -t0` prints offsets on the whole device, not in the partition.
- `vmkfstools` prints progress to standard output; redirect it.
- The disks of our guest offered discard (`blkdiscard`) while the VM had a snapshot; that is how the discard
  entries of SESparse deltas were produced.
