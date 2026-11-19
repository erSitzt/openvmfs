data "vsphere_datacenter" "dc" {
  name = "ha-datacenter" # the fixed name on a standalone ESXi host
}

data "vsphere_host" "esxi" {
  name          = var.esxi_host
  datacenter_id = data.vsphere_datacenter.dc.id
}

data "vsphere_resource_pool" "pool" {
  name          = "Resources"
  datacenter_id = data.vsphere_datacenter.dc.id
}

data "vsphere_network" "net" {
  name          = "VM Network"
  datacenter_id = data.vsphere_datacenter.dc.id
}

# VMFS6 datastores on the test disks. ESXi partitions and formats them the way the vSphere client would.
resource "vsphere_vmfs_datastore" "test" {
  for_each       = var.datastores
  name           = each.key
  host_system_id = data.vsphere_host.esxi.id
  disks          = each.value
}

resource "vsphere_file" "guest_iso" {
  datacenter         = data.vsphere_datacenter.dc.name
  datastore          = vsphere_vmfs_datastore.test[var.iso_datastore].name
  source_file        = var.guest_iso
  destination_file   = "iso/guest.iso"
  create_directories = true
}

resource "vsphere_virtual_machine" "guest" {
  for_each         = var.guests
  name             = each.key
  resource_pool_id = data.vsphere_resource_pool.pool.id
  host_system_id   = data.vsphere_host.esxi.id
  datastore_id     = vsphere_vmfs_datastore.test[each.value.datastore].id

  guest_id  = "otherLinux64Guest"
  num_cpus  = 1
  memory    = 512
  scsi_type = "pvscsi"
  # A live system without tools: nothing to wait for.
  wait_for_guest_net_timeout = 0
  wait_for_guest_ip_timeout  = 0

  network_interface {
    network_id = data.vsphere_network.net.id
  }

  cdrom {
    datastore_id = vsphere_vmfs_datastore.test[var.iso_datastore].id
    path         = vsphere_file.guest_iso.destination_file
  }

  dynamic "disk" {
    for_each = each.value.disks_gb
    content {
      label            = "data${disk.key}"
      size             = disk.value
      thin_provisioned = true
      unit_number      = disk.key
    }
  }
}
