#!/bin/bash
# Common utilities for QEMU-based FAT32 testing
# Source this file from other scripts

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Logging functions
log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_test() {
    echo ""
    echo -e "${BLUE}========================================${NC}"
    echo -e "${BLUE}TEST: $1${NC}"
    echo -e "${BLUE}========================================${NC}"
}

log_phase() {
    echo -e "${YELLOW}>>> $1${NC}"
}

# Check prerequisites for QEMU-based testing
check_qemu_prerequisites() {
    local missing=0

    log_info "Checking prerequisites..."

    # QEMU
    if ! command -v qemu-system-x86_64 &>/dev/null; then
        log_error "qemu-system-x86_64 not found. Please install qemu-system-x86."
        missing=1
    fi

    # KVM access (optional but recommended)
    if [ -e /dev/kvm ]; then
        if [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
            log_info "KVM available - tests will run fast"
        else
            log_warn "KVM exists but not accessible - tests will be slow"
            log_warn "Add yourself to 'kvm' group: sudo usermod -aG kvm $USER"
        fi
    else
        log_warn "KVM not available - tests will use TCG (slower)"
    fi

    # dosfstools
    if ! command -v mkfs.fat &>/dev/null; then
        log_error "mkfs.fat not found. Please install dosfstools."
        missing=1
    fi

    if ! command -v dosfsck &>/dev/null; then
        log_error "dosfsck not found. Please install dosfstools."
        missing=1
    fi

    # mtools (optional)
    if command -v mcopy &>/dev/null; then
        export HAVE_MTOOLS=1
        log_info "mtools found"
    else
        export HAVE_MTOOLS=0
        log_warn "mtools not found - some features may be limited"
    fi

    return $missing
}

# Create a FAT32 image
create_fat32_image() {
    local path="$1"
    local size_mb="$2"

    log_info "Creating ${size_mb}MB FAT32 image at $path"
    truncate -s "${size_mb}M" "$path"
    mkfs.fat -F 32 "$path" >/dev/null 2>&1
}

# Extend an image file
extend_image() {
    local path="$1"
    local new_size_mb="$2"

    log_info "Extending image to ${new_size_mb}MB"
    truncate -s "${new_size_mb}M" "$path"
}

# Run dosfsck on an image
check_filesystem() {
    local path="$1"
    local desc="${2:-filesystem}"

    log_info "Checking filesystem ($desc)..."
    if dosfsck -n "$path" 2>&1; then
        log_info "Filesystem check passed"
        return 0
    else
        log_error "Filesystem check failed"
        return 1
    fi
}

# Clean up work directory
cleanup_work_dir() {
    local work_dir="$1"
    local keep="${2:-false}"

    if [ "$keep" = "true" ] || [ "$keep" = "1" ]; then
        log_info "Keeping work directory: $work_dir"
    else
        log_info "Cleaning up work directory: $work_dir"
        rm -rf "$work_dir"
    fi
}

# Get the fat32expander binary path
# Prefers static (musl) build for VM testing
get_binary_path() {
    local script_dir="$1"
    local project_dir=$(dirname $(dirname "$script_dir"))

    # Prefer static musl binary for VM testing
    local musl_binary="$project_dir/target/x86_64-unknown-linux-musl/release/fat32expander"
    local glibc_binary="$project_dir/target/release/fat32expander"

    if [ -f "$musl_binary" ]; then
        echo "$musl_binary"
        return 0
    fi

    # Try to build static binary
    log_info "Building static fat32expander..."
    if (cd "$project_dir" && cargo build --release --target x86_64-unknown-linux-musl 2>/dev/null); then
        echo "$musl_binary"
        return 0
    fi

    # Fall back to glibc build
    if [ -f "$glibc_binary" ]; then
        log_warn "Using dynamically linked binary - may not work in VM"
        echo "$glibc_binary"
        return 0
    fi

    log_info "Building fat32expander..."
    (cd "$project_dir" && cargo build --release) || {
        log_error "Failed to build fat32expander"
        return 1
    }

    echo "$glibc_binary"
}

# Wait for a file to appear with timeout
wait_for_file() {
    local file="$1"
    local timeout="${2:-60}"
    local interval="${3:-0.5}"
    local elapsed=0

    while [ ! -f "$file" ]; do
        sleep "$interval"
        elapsed=$(echo "$elapsed + $interval" | bc)
        if [ "$(echo "$elapsed >= $timeout" | bc)" -eq 1 ]; then
            return 1
        fi
    done
    return 0
}

# Generate a random seed for deterministic test data
generate_seed() {
    local test_name="$1"
    # Use test name hash for deterministic but unique seed
    echo "$test_name" | md5sum | cut -c1-8
}
