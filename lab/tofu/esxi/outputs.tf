output "vm_path" {
  value = "${var.folder}/${vsphere_virtual_machine.esxi.name}"
}

output "test_devices" {
  description = "Label of each test disk and its device on the nested host. With disk UUIDs enabled, ESXi names a virtual disk after its UUID."
  value = {
    for disk in vsphere_virtual_machine.esxi.disk :
    disk.label => disk.uuid == null ? "(known after apply)" : "naa.${lower(replace(disk.uuid, "-", ""))}" if disk.label != "boot"
  }
}
