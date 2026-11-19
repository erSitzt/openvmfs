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
    fz = ["mpx.vmhba0:C0:T6:L0"]                        # zero
    f1 = ["mpx.vmhba0:C0:T1:L0"]                        # iso
    f2 = ["mpx.vmhba0:C0:T2:L0"]                        # lfb
    fp = ["mpx.vmhba0:C0:T3:L0"]                        # probe
    sp = ["mpx.vmhba0:C0:T4:L0", "mpx.vmhba0:C0:T5:L0"] # spana, spanb
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
