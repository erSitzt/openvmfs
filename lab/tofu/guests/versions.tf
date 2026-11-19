terraform {
  required_version = ">= 1.6"
  required_providers {
    vsphere = {
      source  = "vmware/vsphere"
      version = "~> 2.10"
    }
  }
}

# This stage talks to the nested ESXi host itself, once it is installed.
provider "vsphere" {
  vsphere_server       = var.esxi_host
  user                 = "root"
  password             = var.esxi_password
  allow_unverified_ssl = true # a freshly installed host has a self-signed certificate
}
