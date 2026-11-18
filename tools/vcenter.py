#!/usr/bin/env python3
"""Runs govc against a test vCenter whose address and credentials are in an env file.

Used by the fixture recipes. The env file is named by OPENVMFS_VCENTER_ENV and holds:

    VCENTER_URL=https://vcenter.example
    VCENTER_USER=...
    VCENTER_PASSWORD=...
    VCENTER_SPKI=<SHA-256 of the server's public key, hex>

The server certificate is checked against the pinned public key before any login. Credentials reach govc through
its environment only; nothing secret is printed or written. Keep the env file out of the repository.

Usage: OPENVMFS_VCENTER_ENV=~/.config/.../vcenter.env tools/vcenter.py <govc arguments...>
"""
import hashlib
import os
import ssl
import subprocess
import sys
import tempfile
import urllib.parse



def load_env(path):
    values = {}
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            key, value = line.split("=", 1)
            key = key.strip().removeprefix("export ").strip()
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
                value = value[1:-1]
            values[key] = value
    return values


def main():
    env_file = os.environ.get("OPENVMFS_VCENTER_ENV")
    if not env_file:
        sys.stderr.write("set OPENVMFS_VCENTER_ENV to the env file of the test vCenter\n")
        return 2
    values = load_env(os.path.expanduser(env_file))
    url = values["VCENTER_URL"]
    host = urllib.parse.urlsplit(url if "://" in url else "https://" + url).hostname

    # Fetch the certificate without trusting it, then compare its public key with the pin.
    pem = ssl.get_server_certificate((host, 443))
    der = ssl.PEM_cert_to_DER_cert(pem)
    spki = subprocess.run(
        ["openssl", "x509", "-inform", "DER", "-pubkey", "-noout"], input=der, capture_output=True, check=True
    ).stdout
    spki_der = subprocess.run(
        ["openssl", "pkey", "-pubin", "-outform", "DER"], input=spki, capture_output=True, check=True
    ).stdout
    pin = values.get("VCENTER_SPKI", "").strip().lower()
    actual_hex = hashlib.sha256(spki_der).hexdigest()
    if pin != actual_hex:
        sys.stderr.write(f"certificate pin mismatch for {host}: refusing to log in\n")
        return 2

    thumbprint = ":".join(f"{b:02X}" for b in hashlib.sha256(der).digest())
    with tempfile.NamedTemporaryFile("w", suffix=".known_hosts", delete=False) as known:
        known.write(f"{host} {thumbprint}\n")
    env = {k: v for k, v in os.environ.items() if not k.startswith("GOVC_")}
    env.update(
        GOVC_URL=host,
        GOVC_USERNAME=values["VCENTER_USER"],
        GOVC_PASSWORD=values["VCENTER_PASSWORD"],
        GOVC_TLS_KNOWN_HOSTS=known.name,
    )
    try:
        return subprocess.run(["govc", *sys.argv[1:]], env=env).returncode
    finally:
        os.unlink(known.name)


if __name__ == "__main__":
    sys.exit(main())
