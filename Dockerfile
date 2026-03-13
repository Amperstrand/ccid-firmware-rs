# Reproducible build environment for STM32 CCID firmware
#
# Usage:
#   docker build -t ccid-firmware-builder .
#   docker build --build-arg PROFILE=profile-gemalto-plain -t ccid-firmware-builder .
#   docker create --name extract ccid-firmware-builder
#   docker cp extract:/app/target/thumbv7em-none-eabihf/release/ccid-firmware ./ccid-firmware.elf
#   docker rm extract

FROM rust:1.92-slim-bookworm

# Build argument for device profile (default: profile-cherry-st2100)
ARG PROFILE=profile-cherry-st2100

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
COPY Cargo.toml Cargo.lock ./
COPY build.rs memory.x ./
COPY src ./src
COPY vendor ./vendor
COPY .cargo ./.cargo
COPY rust-toolchain.toml ./

# Build firmware with profile-specific features
RUN if [ "$PROFILE" = "profile-cherry-st2100" ]; then \
      cargo build --release --target thumbv7em-none-eabihf; \
    else \
      cargo build --release --no-default-features --features "$PROFILE" --target thumbv7em-none-eabihf; \
    fi

# Output: /app/target/thumbv7em-none-eabihf/release/ccid-firmware
