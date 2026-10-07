# Reproducible build environment for STM32 CCID firmware
#
# The Rust toolchain is pinned by the base image tag (rust:1.92-slim-bookworm).
#
# Usage:
#   docker build -t ccid-firmware-builder .                            # default features (Cherry ST-2xxx, STM32F469)
#   docker build --build-arg PROFILE=profile-gemalto-idbridge-ct30 -t ccid-firmware-builder .
#   docker build --build-arg PROFILE=stm32f746,profile-cherry-smartterminal-st2xxx -t ccid-firmware-builder .
#   docker create --name extract ccid-firmware-builder
#   docker cp extract:/app/output/ccid-firmware.elf ./ccid-firmware.elf
#   docker cp extract:/app/output/ccid-firmware-default.bin ./ccid-firmware.bin
#   docker rm extract
#
# PROFILE must be "default" or a comma-separated list of cargo features that
# exist in firmware/ccid-firmware/Cargo.toml (validated by
# scripts/check_release_config.sh in CI).

FROM rust:1.92-slim-bookworm

# Build argument for device profile: "default" uses the manifest default
# feature set; anything else is passed as --features with --no-default-features.
ARG PROFILE=default

# Install ARM cross-compilation toolchain
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc-arm-none-eabi \
    binutils-arm-none-eabi \
    && rm -rf /var/lib/apt/lists/*

# Set SOURCE_DATE_EPOCH for reproducible builds (Jan 1, 2024)
ENV SOURCE_DATE_EPOCH=1704067200

# Add Rust target
RUN rustup target add thumbv7em-none-eabihf

WORKDIR /app

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock README.md ./
COPY crates ./crates
COPY firmware ./firmware
COPY host-tools ./host-tools
COPY vendor ./vendor
COPY .cargo ./.cargo

# Build firmware with profile-specific features
RUN if [ "$PROFILE" = "default" ]; then \
      cargo build --release -p ccid-firmware-rs --target thumbv7em-none-eabihf; \
    else \
      cargo build --release -p ccid-firmware-rs --no-default-features --features "$PROFILE" --target thumbv7em-none-eabihf; \
    fi && \
    mkdir -p /app/output && \
    cp target/thumbv7em-none-eabihf/release/ccid-firmware /app/output/ccid-firmware.elf && \
    arm-none-eabi-objcopy -O binary target/thumbv7em-none-eabihf/release/ccid-firmware /app/output/ccid-firmware-${PROFILE}.bin && \
    sha256sum /app/output/ccid-firmware-${PROFILE}.bin > /app/output/ccid-firmware-${PROFILE}.bin.sha256

# Output: /app/output/ccid-firmware.elf, /app/output/ccid-firmware-${PROFILE}.bin(.sha256)
