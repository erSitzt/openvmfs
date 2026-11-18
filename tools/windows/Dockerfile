# Cross-compiles OpenVmfs for Windows x64 (x86_64-pc-windows-gnu) on Linux.
# Used by tools/build-windows.sh; CI builds on a Windows runner instead.
ARG BASE=rust:1-bookworm
FROM ${BASE}
ARG TOOLCHAIN=stable
RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/*
RUN rustup toolchain install ${TOOLCHAIN} --profile minimal --target x86_64-pc-windows-gnu \
    && chmod -R a+rwX /usr/local/rustup /usr/local/cargo
