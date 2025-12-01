#!/bin/bash
#
# Setup Alpine Linux kernel and initramfs for QEMU testing
#
# Downloads Alpine's netboot files which include:
# - vmlinuz-virt: kernel with vfat support built-in
# - initramfs-virt: minimal initramfs with busybox
#

set -e

ALPINE_VERSION="3.20"
ALPINE_ARCH="x86_64"
ALPINE_MIRROR="https://dl-cdn.alpinelinux.org/alpine"

# Cache directory
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/fat32expander-test"

# Files to download
KERNEL_FILE="vmlinuz-virt"
INITRD_FILE="initramfs-virt"

# URLs
BASE_URL="${ALPINE_MIRROR}/v${ALPINE_VERSION}/releases/${ALPINE_ARCH}/netboot"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# Check if files already exist and are valid
check_existing() {
    if [ -f "$CACHE_DIR/$KERNEL_FILE" ] && [ -f "$CACHE_DIR/$INITRD_FILE" ]; then
        # Quick sanity check - kernel should be > 5MB, initrd > 5MB
        local kernel_size=$(stat -c%s "$CACHE_DIR/$KERNEL_FILE" 2>/dev/null || echo 0)
        local initrd_size=$(stat -c%s "$CACHE_DIR/$INITRD_FILE" 2>/dev/null || echo 0)

        if [ "$kernel_size" -gt 5000000 ] && [ "$initrd_size" -gt 5000000 ]; then
            return 0
        fi
    fi
    return 1
}

# Download a file with progress
download_file() {
    local url="$1"
    local dest="$2"
    local name=$(basename "$dest")

    log_info "Downloading $name..."

    if command -v curl &>/dev/null; then
        curl -L --progress-bar -o "$dest" "$url"
    elif command -v wget &>/dev/null; then
        wget --show-progress -q -O "$dest" "$url"
    else
        log_error "Neither curl nor wget found"
        exit 1
    fi
}

# Main setup function
setup_alpine() {
    log_info "Setting up Alpine Linux for QEMU testing"
    log_info "Alpine version: $ALPINE_VERSION"
    log_info "Cache directory: $CACHE_DIR"

    # Check if already set up
    if check_existing; then
        log_info "Alpine kernel and initramfs already cached"
        log_info "  Kernel: $CACHE_DIR/$KERNEL_FILE"
        log_info "  Initrd: $CACHE_DIR/$INITRD_FILE"
        return 0
    fi

    # Create cache directory
    mkdir -p "$CACHE_DIR"

    # Download kernel
    download_file "${BASE_URL}/${KERNEL_FILE}" "$CACHE_DIR/$KERNEL_FILE"

    # Download initramfs
    download_file "${BASE_URL}/${INITRD_FILE}" "$CACHE_DIR/$INITRD_FILE"

    # Verify downloads
    if ! check_existing; then
        log_error "Downloaded files appear to be invalid"
        exit 1
    fi

    log_info "Alpine setup complete!"
    log_info "  Kernel: $CACHE_DIR/$KERNEL_FILE"
    log_info "  Initrd: $CACHE_DIR/$INITRD_FILE"
}

# Print paths for other scripts to source
print_paths() {
    echo "ALPINE_KERNEL=$CACHE_DIR/$KERNEL_FILE"
    echo "ALPINE_INITRD=$CACHE_DIR/$INITRD_FILE"
}

# Parse arguments
case "${1:-setup}" in
    setup)
        setup_alpine
        ;;
    paths)
        if check_existing; then
            print_paths
        else
            log_error "Alpine not set up. Run: $0 setup"
            exit 1
        fi
        ;;
    check)
        if check_existing; then
            log_info "Alpine is set up and ready"
            print_paths
            exit 0
        else
            log_warn "Alpine not set up"
            exit 1
        fi
        ;;
    clean)
        log_info "Removing cached Alpine files..."
        rm -f "$CACHE_DIR/$KERNEL_FILE" "$CACHE_DIR/$INITRD_FILE"
        log_info "Done"
        ;;
    *)
        echo "Usage: $0 [setup|paths|check|clean]"
        echo ""
        echo "Commands:"
        echo "  setup  - Download Alpine kernel and initramfs (default)"
        echo "  paths  - Print paths to cached files"
        echo "  check  - Check if Alpine is set up"
        echo "  clean  - Remove cached files"
        exit 1
        ;;
esac
