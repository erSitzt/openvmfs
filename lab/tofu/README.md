# The test lab as OpenTofu / Terraform configuration

Two stages that build the lab described in [`docs/internals/lab.md`](../../docs/internals/lab.md): a nested ESXi
host with thin test disks, then test datastores and small guest VMs on it. They work with `tofu` and with
`terraform`.

**Status: both stages have been applied.** Stage 1 built the lab's ESXi 8.0 U3 host (installed by hand through the
console afterwards); stage 2 created its test datastores, one of them on two devices, and uploaded the guest ISO. On
an ESXi 7.0.3 host stage 2 also started a guest. A
second `plan` showed no changes in both. The scripted installation of ESXi between the stages was not tried.

## Stage 1: the nested host (`esxi/`)

Creates one VM in your vSphere: 4 CPUs, 8 GiB, hardware virtualisation passed through, a 40 GiB boot disk and one
thin disk per entry of `test_disks_gb`, with the ESXi installer ISO in its CD drive.

```bash
cd lab/tofu/esxi
cp terraform.tfvars.example terraform.tfvars      # edit
export TF_VAR_vcenter_password=...
tofu init && tofu apply
```

What it does not do: install ESXi. Either install by hand through the VM's console, or build an installer ISO
that carries a kickstart file (`ks.cfg.example`; VMware documents the procedure as a "scripted installation") and
point `esxi_iso` to it. After the installation the host has an address from DHCP; evaluation mode is enough.

Set `host` when the datastore is local to one host of the cluster; otherwise vCenter may place the VM on a host
that cannot see it.

The account needs the right to create VMs in the folder, to use the datastore and network, and to take snapshots
of the VM (the consistency experiments use them).

## Stage 2: datastores and guests (`guests/`)

Talks to the nested host directly. Creates the VMFS6 datastores on the test disks (two devices in one entry make
a datastore that spans devices), uploads a small Linux live ISO and creates the guest VMs with empty data disks.

```bash
cd lab/tofu/guests
export TF_VAR_esxi_password=...
tofu init && tofu apply -var esxi_host=192.0.2.10 -var guest_iso=$HOME/Downloads/alpine-virt-3.20.0-x86_64.iso
```

The device names in `datastores` must match the `test_devices` output of stage 1. The host must offer a disk for it: a device that was detached on this host
before (`esxcli storage core device detached list`) is not offered until that record is removed. The disks in the
test were 4 GiB; smaller ones were not tried through the API.

## What stays manual

- **Junk under the test disks and automatic unmap off** (`esxcli storage vmfs reclaim config set
  --reclaim-priority=none`), creating test disks with `vmkfstools`, writing pattern data, recording ESXi's view
  and capturing the devices: the recipes in `fixtures/recipes/` and the steps in `docs/internals/lab.md`.
- **A 17 TiB disk** for the large-volume fixture. Add `big = 17408` to `test_disks_gb` only while you need it.
- **Driving the guests**: they have no network and no tools; `govc vm.keystrokes` types into their console.

## Removing it

`tofu destroy` in `guests/`, then in `esxi/`. The second one deletes the VM with all its disks.
