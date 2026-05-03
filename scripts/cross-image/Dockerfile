# Dockerfile for cross-building nomopractic for aarch64 with native DBus headers
# Based on the official rustembedded/cross image for aarch64

FROM rustembedded/cross:aarch64-unknown-linux-gnu

USER root

ENV DEBIAN_FRONTEND=noninteractive

# Install native development headers and pkg-config so libdbus-sys can find dbus.pc
RUN dpkg --add-architecture arm64 \
 && apt-get update \
 && apt-get install -y --no-install-recommends \
    pkg-config \
    libdbus-1-dev:arm64 \
    libssl-dev:arm64 \
    libudev-dev:arm64 \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/*

# Leave working dir to the repo mount point that cross will provide at runtime
WORKDIR /work

# Ensure pkg-config paths exist for aarch64 cross builds
RUN mkdir -p /usr/aarch64-linux-gnu/lib /usr/aarch64-linux-gnu/usr/lib/aarch64-linux-gnu \
 && ln -sfn /usr/lib/aarch64-linux-gnu/pkgconfig /usr/aarch64-linux-gnu/lib/pkgconfig \
 && ln -sfn /usr/lib/aarch64-linux-gnu/pkgconfig /usr/aarch64-linux-gnu/usr/lib/aarch64-linux-gnu/pkgconfig || true

# Make pkg-config find aarch64 .pc files by default
ENV PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig

# The cross tool will mount the project and set PKG_CONFIG_LIBDIR appropriately
# No entrypoint - this image is intended for use as CROSS_CONTAINER with `cross`.
