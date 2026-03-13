#!/bin/bash
# Reproducibility Verification Script
# Builds firmware twice in different paths and compares hashes
#
# Usage:
#   ./scripts/verify-reproducibility.sh <profile>
#
# Example:
#   ./scripts/verify-reproducibility.sh profile-cherry-st2100
#
# Exit codes:
#   0 - Hashes match (reproducible)
#   1 - Hashes differ (NOT reproducible)
#   2 - Build failed

set -euo pipefail

PROFILE="${1:-profile-cherry-st2100}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
WORK_DIR="${PROJECT_ROOT}/.reproducibility-test"

echo "=== Reproducibility Verification ==="
echo "Profile: ${PROFILE}"
echo "Work directory: ${WORK_DIR}"
echo ""

# Cleanup previous test runs
rm -rf "${WORK_DIR}"
mkdir -p "${WORK_DIR}/build1" "${WORK_DIR}/build2"

# Function to build and extract hash
build_and_hash() {
    local BUILD_NUM="$1"
    local OUTPUT_DIR="${WORK_DIR}/build${BUILD_NUM}"
    local CONTAINER_NAME="ccid-verify-${BUILD_NUM}"
    
    echo "--- Build ${BUILD_NUM} ---"
    
    # Build Docker image
    docker build \
        --build-arg PROFILE="${PROFILE}" \
        --tag "ccid-firmware-verify:${BUILD_NUM}" \
        --no-cache \
        "${PROJECT_ROOT}" > "${OUTPUT_DIR}/docker-build.log" 2>&1
    
    if [ $? -ne 0 ]; then
        echo "ERROR: Docker build ${BUILD_NUM} failed. See ${OUTPUT_DIR}/docker-build.log"
        return 2
    fi
    
    # Extract artifacts
    docker create --name "${CONTAINER_NAME}" "ccid-firmware-verify:${BUILD_NUM}" > /dev/null
    docker cp "${CONTAINER_NAME}:/app/output/." "${OUTPUT_DIR}/"
    docker rm "${CONTAINER_NAME}" > /dev/null
    
    # Get hash
    local HASH_FILE="${OUTPUT_DIR}/ccid-firmware-${PROFILE}.bin.sha256"
    if [ ! -f "${HASH_FILE}" ]; then
        echo "ERROR: Hash file not found: ${HASH_FILE}"
        return 2
    fi
    
    cat "${HASH_FILE}"
    return 0
}

# Build 1
echo "Starting first build..."
HASH1=$(build_and_hash 1 | awk '{print $1}')
BUILD1_RESULT=$?

if [ $BUILD1_RESULT -ne 0 ]; then
    echo "Build 1 failed with exit code ${BUILD1_RESULT}"
    rm -rf "${WORK_DIR}"
    exit 2
fi

echo "Build 1 hash: ${HASH1}"
echo ""

# Build 2
echo "Starting second build..."
HASH2=$(build_and_hash 2 | awk '{print $1}')
BUILD2_RESULT=$?

if [ $BUILD2_RESULT -ne 0 ]; then
    echo "Build 2 failed with exit code ${BUILD2_RESULT}"
    rm -rf "${WORK_DIR}"
    exit 2
fi

echo "Build 2 hash: ${HASH2}"
echo ""

# Compare hashes
echo "=== Hash Comparison ==="
echo "Build 1: ${HASH1}"
echo "Build 2: ${HASH2}"
echo ""

if [ "${HASH1}" = "${HASH2}" ]; then
    echo "✅ SUCCESS: Hashes match! Build is reproducible."
    echo ""
    echo "Evidence:"
    echo "  Build 1 artifact: ${WORK_DIR}/build1/ccid-firmware-${PROFILE}.bin"
    echo "  Build 2 artifact: ${WORK_DIR}/build2/ccid-firmware-${PROFILE}.bin"
    echo "  Hash: ${HASH1}"
    rm -rf "${WORK_DIR}"
    exit 0
else
    echo "❌ FAILURE: Hashes differ! Build is NOT reproducible."
    echo ""
    echo "This indicates non-deterministic behavior in the build process."
    echo "Check for:"
    echo "  - Timestamps in build output"
    echo "  - Random values in code generation"
    echo "  - Unordered hash maps or sets"
    echo "  - Absolute paths in debug info"
    echo ""
    echo "Artifacts preserved at:"
    echo "  ${WORK_DIR}/build1/"
    echo "  ${WORK_DIR}/build2/"
    exit 1
fi
