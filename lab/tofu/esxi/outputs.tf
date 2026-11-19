output "vm_path" {
  value = "${var.folder}/${vsphere_virtual_machine.esxi.name}"
}

output "test_devices" {
  description = "Label of each test disk and its device on the nested host, as ESXi names virtual disks on the first controller."
  value       = { for index, disk in local.test_disks : disk.label => "mpx.vmhba0:C0:T${index + 1}:L0" }
}
