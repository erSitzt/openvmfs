variable "esxi_host" {
  description = "Address of the nested ESXi host (it gets one by DHCP; read it from its console or from vCenter)."
  type        = string
}

variable "esxi_password" {
  description = "Root password of the nested host, as set in the kickstart file."
  type        = string
  sensitive   = true
}

variable "datastores" {
  description = "Test datastores: label => devices. More than one device makes a datastore that spans devices. Device names are the `test_devices` output of the esxi stage."
  type        = map(list(string))
  default = {
    # Replace the device names with the ones stage 1 printed.
    fz = ["naa.zero"]
    f1 = ["naa.iso"]
    f2 = ["naa.lfb"]
    fp = ["naa.probe"]
    sp = ["naa.spana", "naa.spanb"]
  }
}

variable "guest_iso" {
  description = "Local path of a small Linux live ISO for the guests, for example Alpine's `alpine-virt-*.iso`."
  type        = string
}

variable "iso_datastore" {
  description = "Label of the datastore that takes the ISO."
  type        = string
  default     = "f1"
}

variable "guests" {
  description = "Guest VMs: name => datastore for the VM and sizes (GiB) of its empty thin data disks. The guests boot the live ISO; they are driven through the console (docs/internals/lab.md)."
  type = map(object({
    datastore = string
    disks_gb  = list(number)
  }))
  default = {
    ovmfs-guest = { datastore = "fz", disks_gb = [1, 1] }
    ovmfs-probe = { datastore = "fp", disks_gb = [1] }
  }
}
