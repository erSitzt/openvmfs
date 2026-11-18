# Comparison tools for OpenVmfs tests. They are used as black boxes to cross-check results and are never linked
# into, vendored into or shipped with OpenVmfs.
#
#   dissect.vmfs (AGPL-3.0)   independent VMFS5/6 reader in Python
#   vmfs6-tools  (GPL-2.0+)   debugvmfs6, the older C reader
#   qemu-utils   (GPL-2.0)    qemu-img, for VMDK snapshot chains
#
# Build:  docker build -t openvmfs-oracle tools/oracle
# Use:    docker run --rm -v "$PWD/target/fixture-cache:/data:ro" openvmfs-oracle manifest /data/dissect/vmfs6.bin
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 python3-venv vmfs6-tools qemu-utils ca-certificates \
    && rm -rf /var/lib/apt/lists/*

RUN python3 -m venv /opt/oracle \
    && /opt/oracle/bin/pip install --no-cache-dir dissect.vmfs

COPY oracle.py /usr/local/bin/oracle.py
ENTRYPOINT ["/opt/oracle/bin/python", "-I", "/usr/local/bin/oracle.py"]
