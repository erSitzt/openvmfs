data "vsphere_datacenter" "dc" {
  name = var.datacenter
}

data "vsphere_compute_cluster" "cluster" {
  name          = var.cluster
  datacenter_id = data.vsphere_datacenter.dc.id
}

data "vsphere_host" "host" {
  count         = var.host == null ? 0 : 1
  name          = var.host
  datacenter_id = data.vsphere_datacenter.dc.id
}

data "vsphere_datastore" "ds" {
  name          = var.datastore
  datacenter_id = data.vsphere_datacenter.dc.id
}

data "vsphere_datastore" "iso" {
  name          = coalesce(var.iso_datastore, var.datastore)
  datacenter_id = data.vsphere_datacenter.dc.id
}

data "vsphere_network" "net" {
  name          = var.network
  datacenter_id = data.vsphere_datacenter.dc.id
}

locals {
  # A stable order, so that a disk keeps its SCSI unit number when another one is added at the end.
  test_disks = [for label in sort(keys(var.test_disks_gb)) : { label = label, size = var.test_disks_gb[label] }]
}

# The nested ESXi host: a VM with hardware virtualisation passed through to the guest.
resource "vsphere_virtual_machine" "esxi" {
  name             = var.name
  folder           = var.folder
  resource_pool_id = data.vsphere_compute_cluster.cluster.resource_pool_id
  host_system_id   = one(data.vsphere_host.host[*].id)
  datastore_id     = data.vsphere_datastore.ds.id

  guest_id                   = "vmkernel7Guest"
  num_cpus                   = 4
  memory                     = 8192
  nested_hv_enabled          = true
  firmware                   = "efi"
  scsi_type                  = "pvscsi"
  enable_disk_uuid           = true
  wait_for_guest_net_timeout = 0
  wait_for_guest_ip_timeout  = 0

  network_interface {
    network_id   = data.vsphere_network.net.id
    adapter_type = "vmxnet3"
  }

  cdrom {
    datastore_id = data.vsphere_datastore.iso.id
    path         = var.esxi_iso
  }

  disk {
    label            = "boot"
    size             = 40
    thin_provisioned = true
    unit_number      = 0
  }

  dynamic "disk" {
    for_each = local.test_disks
    content {
      label            = disk.value.label
      size             = disk.value.size
      thin_provisioned = true
      unit_number      = disk.key + 1
    }
  }

  lifecycle {
    # The installer ISO is ejected by hand or by the install; do not fight over it.
    ignore_changes = [cdrom]
  }
}
