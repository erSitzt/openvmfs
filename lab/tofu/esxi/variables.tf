variable "vcenter_server" {
  description = "vCenter (or ESXi host) that will run the nested ESXi VM."
  type        = string
}

variable "vcenter_user" {
  type = string
}

variable "vcenter_password" {
  type      = string
  sensitive = true
}

variable "allow_unverified_ssl" {
  description = "Accept a certificate that cannot be verified. Prefer installing the CA instead."
  type        = bool
  default     = false
}

variable "datacenter" {
  type = string
}

variable "cluster" {
  description = "Cluster (or standalone host's compute resource) to run the VM on."
  type        = string
}

variable "host" {
  description = "ESXi host to place the VM on. Needed when the datastore is local to one host of the cluster."
  type        = string
  default     = null
}

variable "datastore" {
  description = "Datastore for the VM and its thin test disks."
  type        = string
}

variable "network" {
  description = "Port group for the nested host's management interface. DHCP is expected."
  type        = string
}

variable "folder" {
  description = "VM folder to work in. Use a dedicated one."
  type        = string
}

variable "name" {
  type    = string
  default = "openvmfs-esxi01"
}

variable "esxi_iso" {
  description = "Path of the ESXi installer ISO on a datastore, for example \"ISO/VMware-VMvisor-Installer-7.0U3.iso\". Use an ISO with a kickstart file for an unattended install (see README)."
  type        = string
}

variable "iso_datastore" {
  description = "Datastore that holds the ISO. Defaults to var.datastore."
  type        = string
  default     = null
}

variable "test_disks_gb" {
  description = "Thin disks that become test datastores, by label. Large ones cost almost nothing while they stay empty; delete a 17 TiB disk as soon as its fixture is captured."
  type        = map(number)
  default = {
    zero  = 4   # base disks: thin, lazy, eager
    iso   = 16  # guest ISO and clones
    lfb   = 256 # large file blocks need more than 128 GiB
    probe = 4   # consistency experiments: small, because it is downloaded whole every run
    spana = 4   # a datastore on two devices
    spanb = 4   # the vSphere API wants at least 4 GiB for a VMFS6 device
  }
}
