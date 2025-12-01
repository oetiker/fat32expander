#!/bin/bash
#
# Crash Recovery Test for fat32expander
#
# Tests that fat32expander can recover from crashes at ALL injection points:
#   - after_checkpoint_start
#   - after_data_shift
#   - after_checkpoint_data_copied
#   - after_boot_invalidate
#   - after_fat_write
#   - after_checkpoint_fat_written
#
# For each crash point:
# 1. Creates FAT32 image with test data
# 2. Triggers crash via fault injection
# 3. Verifies boot sector state (invalidated when appropriate)
# 4. Runs fat32expander again - should recover
# 5. Verifies filesystem integrity and all files
#
# Usage: ./test-crash-recovery.sh
#

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$SCRIPT_DIR")")"

source "$SCRIPT_DIR/lib/common.sh"

WORK_DIR="${TMPDIR:-/tmp}/fat32expander-crash-test-$$"
VM_TIMEOUT=300  # Longer timeout for testing all crash points

cleanup() {
    [ -d "$WORK_DIR" ] && rm -rf "$WORK_DIR"
}
trap cleanup EXIT

# Ensure prerequisites
ensure_prerequisites() {
    if ! check_qemu_prerequisites; then
        log_error "Prerequisites not met"
        exit 1
    fi

    log_info "Checking Alpine setup..."
    if ! "$SCRIPT_DIR/setup-alpine.sh" check >/dev/null 2>&1; then
        log_info "Setting up Alpine Linux..."
        "$SCRIPT_DIR/setup-alpine.sh" setup
    fi
    eval $("$SCRIPT_DIR/setup-alpine.sh" paths)
    export ALPINE_KERNEL ALPINE_INITRD

    # Build with fault-injection feature for crash testing
    log_info "Building with fault-injection feature..."
    if ! cargo build --release --features fault-injection --target x86_64-unknown-linux-musl -q 2>&1; then
        log_error "Failed to build with fault-injection feature"
        exit 1
    fi

    BINARY="$PROJECT_DIR/target/x86_64-unknown-linux-musl/release/fat32expander"
    if [ ! -x "$BINARY" ]; then
        log_error "Binary not found at $BINARY"
        exit 1
    fi
    export BINARY

    log_info "Using kernel: $ALPINE_KERNEL"
    log_info "Using binary: $BINARY (with fault-injection)"
}

# Download Alpine package
download_alpine_pkg() {
    local pkg="$1"
    local cache_dir="${XDG_CACHE_HOME:-$HOME/.cache}/fat32expander-test/apk"
    local pkg_file="$cache_dir/$pkg.apk"

    mkdir -p "$cache_dir"

    if [ ! -f "$pkg_file" ]; then
        echo "Downloading Alpine package: $pkg..."
        local url="https://dl-cdn.alpinelinux.org/alpine/v3.20/main/x86_64"
        curl -sL "$url/APKINDEX.tar.gz" | tar -xzO APKINDEX | \
            grep -A1 "^P:$pkg$" | grep "^V:" | head -1 | cut -d: -f2 > "$cache_dir/$pkg.version"
        local version=$(cat "$cache_dir/$pkg.version")
        curl -sL -o "$pkg_file" "$url/${pkg}-${version}.apk"
    fi

    echo "$pkg_file"
}

# Extract binaries from Alpine package
extract_alpine_pkg() {
    local pkg_file="$1"
    local dest_dir="$2"
    tar -xzf "$pkg_file" -C "$dest_dir" --warning=no-unknown-keyword 2>&1 | grep -v "Ignoring unknown" || true
}

# Create the VM overlay with crash-recovery init
create_crash_overlay() {
    local overlay_dir="$WORK_DIR/overlay"
    rm -rf "$overlay_dir"
    mkdir -p "$overlay_dir"/{bin,sbin,usr/sbin,scripts}

    cp "$BINARY" "$overlay_dir/bin/fat32expander"
    chmod +x "$overlay_dir/bin/fat32expander"

    # Add dosfstools
    local dosfstools_pkg=$(download_alpine_pkg "dosfstools")
    extract_alpine_pkg "$dosfstools_pkg" "$overlay_dir"
    chmod +x "$overlay_dir/sbin/"* 2>/dev/null || true

    # Create crash-recovery test init that tests ALL crash points
    cat > "$overlay_dir/init" << 'INIT_SCRIPT'
#!/bin/busybox sh
# Crash Recovery Test Init
# Tests ALL fault injection crash points

/bin/busybox mkdir -p /proc /sys /dev /tmp /mnt
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs devtmpfs /dev
/bin/busybox mount -t tmpfs tmpfs /tmp

exec > /dev/ttyS0 2>&1

echo "=== Crash Recovery Test Starting ==="
echo "Testing ALL fault injection crash points"
echo ""

/bin/busybox --install -s /bin 2>/dev/null || true

[ -e /dev/loop0 ] || mknod /dev/loop0 b 7 0
[ -e /dev/loop1 ] || mknod /dev/loop1 b 7 1

# Use sizes that ensure valid FAT32 (needs >= 65525 clusters)
INITIAL_SIZE_MB=64
FINAL_SIZE_MB=128

MOUNT_POINT="/mnt/fat"
mkdir -p "$MOUNT_POINT"

# All crash points to test (in order they occur during resize)
CRASH_POINTS="after_checkpoint_start after_data_shift after_checkpoint_data_copied after_boot_invalidate after_fat_write after_checkpoint_fat_written"

# Track results
TESTS_PASSED=0
TESTS_FAILED=0
FAILED_POINTS=""

# Cleanup any existing test images
cleanup_images() {
    rm -f /tmp/test_*.img /tmp/*.checkpoint 2>/dev/null
    # Also unmount if still mounted
    umount "$MOUNT_POINT" 2>/dev/null || true
}

# Create base test image with files
create_test_image() {
    local image="$1"

    # Ensure clean state
    rm -f "$image" 2>/dev/null
    umount "$MOUNT_POINT" 2>/dev/null || true

    dd if=/dev/zero of="$image" bs=1M count="$INITIAL_SIZE_MB" 2>/dev/null
    /sbin/mkfs.fat -F 32 -s 1 "$image" >/dev/null

    mount -t vfat -o loop "$image" "$MOUNT_POINT"

    cd "$MOUNT_POINT"
    echo "Important data file 1" > file1.txt
    echo "Important data file 2" > file2.txt
    mkdir -p subdir
    echo "Nested file content" > subdir/nested.txt

    # Create fewer files to reduce tmpfs usage (20 instead of 100)
    for i in $(seq 1 20); do
        dd if=/dev/urandom of="data_$i.bin" bs=512 count=4 2>/dev/null
    done
    dd if=/dev/urandom of=subdir/binary.bin bs=1024 count=50 2>/dev/null
    sync

    # Record checksums
    find . -type f -exec sha256sum {} \; 2>/dev/null | sort > /tmp/pre_checksums.txt

    cd /
    sync
    umount "$MOUNT_POINT"

    # Extend image
    dd if=/dev/zero bs=1M count=$((FINAL_SIZE_MB - INITIAL_SIZE_MB)) >> "$image" 2>/dev/null
}

# Test a single crash point
test_crash_point() {
    local crash_point="$1"
    local image="/tmp/test_${crash_point}.img"

    echo ""
    echo "========================================"
    echo "Testing crash point: $crash_point"
    echo "========================================"

    # Create fresh test image
    echo "Creating test image..."
    create_test_image "$image"

    # Phase 1: Trigger crash
    echo "Triggering crash at $crash_point..."
    FAT32_CRASH_AT="$crash_point" /bin/fat32expander resize --verbose --force "$image" > /tmp/crash_${crash_point}.log 2>&1
    CRASH_EXIT=$?
    echo "Crash exit code: $CRASH_EXIT"

    # Check boot sector state
    BOOT_SIG=$(dd if="$image" bs=1 skip=510 count=2 2>/dev/null | xxd -p)
    echo "Boot signature after crash: $BOOT_SIG"

    # Determine expected state based on crash point
    # Boot sector is invalidated BEFORE FAT write (after_boot_invalidate)
    # and restored AFTER checkpoint_fat_written
    case "$crash_point" in
        after_checkpoint_start|after_data_shift|after_checkpoint_data_copied)
            # Crash before boot invalidation - boot sector should be valid
            if [ "$BOOT_SIG" != "55aa" ]; then
                echo "WARNING: Boot sector unexpectedly invalidated at $crash_point"
            fi
            ;;
        after_boot_invalidate|after_fat_write)
            # Crash in danger zone - boot sector should be invalidated
            if [ "$BOOT_SIG" != "0000" ]; then
                echo "ERROR: Boot sector should be invalidated (0000) at $crash_point but is $BOOT_SIG"
                return 1
            fi
            echo "Boot sector correctly invalidated (0000)"
            ;;
        after_checkpoint_fat_written)
            # Crash after FAT write but before boot restore - still invalidated
            if [ "$BOOT_SIG" != "0000" ]; then
                echo "WARNING: Boot sector should still be invalidated at $crash_point"
            fi
            ;;
    esac

    # Phase 2: Recovery
    echo "Attempting recovery..."
    /bin/fat32expander resize --verbose --force "$image" > /tmp/recover_${crash_point}.log 2>&1
    RECOVER_EXIT=$?

    if [ $RECOVER_EXIT -ne 0 ]; then
        echo "ERROR: Recovery failed with exit code $RECOVER_EXIT"
        cat /tmp/recover_${crash_point}.log
        return 1
    fi
    echo "Recovery completed (exit $RECOVER_EXIT)"

    # Phase 3: Verify filesystem integrity
    echo "Verifying filesystem integrity..."
    /sbin/fsck.fat -n "$image" > /tmp/fsck_${crash_point}.log 2>&1
    FSCK_EXIT=$?
    if [ $FSCK_EXIT -ne 0 ]; then
        echo "ERROR: fsck.fat failed after recovery"
        cat /tmp/fsck_${crash_point}.log
        return 1
    fi
    echo "fsck.fat passed"

    # Phase 4: Verify file integrity
    echo "Verifying file integrity..."
    mount -t vfat -o loop "$image" "$MOUNT_POINT"
    if [ $? -ne 0 ]; then
        echo "ERROR: Failed to mount after recovery"
        return 1
    fi

    cd "$MOUNT_POINT"
    find . -type f -exec sha256sum {} \; 2>/dev/null | sort > /tmp/post_checksums.txt
    cd /
    umount "$MOUNT_POINT"

    if ! diff -q /tmp/pre_checksums.txt /tmp/post_checksums.txt >/dev/null 2>&1; then
        echo "ERROR: File checksums don't match after recovery!"
        diff /tmp/pre_checksums.txt /tmp/post_checksums.txt
        return 1
    fi

    FILE_COUNT=$(wc -l < /tmp/post_checksums.txt)
    echo "SUCCESS: All $FILE_COUNT files verified for crash point: $crash_point"

    # Cleanup
    rm -f "$image" "${image}.checkpoint" 2>/dev/null
    return 0
}

# Cleanup function for failed tests
cleanup_after_test() {
    local crash_point="$1"
    local image="/tmp/test_${crash_point}.img"
    umount "$MOUNT_POINT" 2>/dev/null || true
    rm -f "$image" "${image}.checkpoint" 2>/dev/null || true
}

# Run all tests
echo "Starting crash recovery tests for all injection points..."
echo "Crash points to test: $CRASH_POINTS"
echo ""

# Initial cleanup
cleanup_images

for crash_point in $CRASH_POINTS; do
    if test_crash_point "$crash_point"; then
        TESTS_PASSED=$((TESTS_PASSED + 1))
        echo "=== PASSED: $crash_point ==="
    else
        TESTS_FAILED=$((TESTS_FAILED + 1))
        FAILED_POINTS="$FAILED_POINTS $crash_point"
        echo "=== FAILED: $crash_point ==="
    fi
    # Always cleanup between tests
    cleanup_after_test "$crash_point"
done

echo ""
echo "========================================"
echo "=== CRASH RECOVERY TEST SUMMARY ==="
echo "========================================"
echo "Total crash points tested: $((TESTS_PASSED + TESTS_FAILED))"
echo "Passed: $TESTS_PASSED"
echo "Failed: $TESTS_FAILED"

if [ $TESTS_FAILED -gt 0 ]; then
    echo "Failed crash points:$FAILED_POINTS"
    echo "GUEST_ERROR: Some crash recovery tests failed"
    echo o > /proc/sysrq-trigger
    sleep 999
fi

echo ""
echo "=== ALL CRASH POINTS PASSED ==="
echo "=== VM FINISHED ==="
sync
sleep 1
echo o > /proc/sysrq-trigger
sleep 999
INIT_SCRIPT
    chmod +x "$overlay_dir/init"

    # Create cpio archive
    (cd "$overlay_dir" && find . | cpio -o -H newc 2>/dev/null) | gzip > "$WORK_DIR/overlay.cpio.gz"
    cat "$ALPINE_INITRD" "$WORK_DIR/overlay.cpio.gz" > "$WORK_DIR/combined-initrd"
    rm -rf "$overlay_dir"
}

run_vm() {
    local serial_log="$WORK_DIR/vm-console.log"

    # Check for KVM
    KVM_OPTS=""
    if [ -e /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
        KVM_OPTS="-enable-kvm -cpu host"
        log_info "Using KVM acceleration"
    else
        log_info "KVM not available, using TCG"
    fi

    log_info "Booting VM..."
    timeout "$VM_TIMEOUT" qemu-system-x86_64 \
        $KVM_OPTS \
        -m 512 \
        -nographic \
        -kernel "$ALPINE_KERNEL" \
        -initrd "$WORK_DIR/combined-initrd" \
        -append "console=ttyS0 quiet" \
        -no-reboot \
        2>&1 | tee "$serial_log"

    QEMU_EXIT=$?
    echo ""
    log_info "VM finished (exit code: $QEMU_EXIT)"

    # Check for errors
    if grep -q "GUEST_ERROR" "$serial_log"; then
        log_error "Guest reported an error:"
        grep "GUEST_ERROR" "$serial_log"
        return 1
    fi

    if ! grep -q "=== VM FINISHED ===" "$serial_log"; then
        log_error "VM did not finish normally (timeout or crash)"
        return 1
    fi

    if ! grep -q "=== ALL CRASH POINTS PASSED ===" "$serial_log"; then
        log_error "Not all crash points passed"
        return 1
    fi

    # Extract summary
    log_info "Crash recovery test summary:"
    grep -A5 "=== CRASH RECOVERY TEST SUMMARY ===" "$serial_log" | tail -4

    return 0
}

main() {
    echo "=============================================="
    echo "fat32expander Crash Recovery Test"
    echo "Testing ALL fault injection points"
    echo "=============================================="

    ensure_prerequisites
    mkdir -p "$WORK_DIR"

    log_test "crash-recovery-all-points"
    log_info "Work directory: $WORK_DIR"

    log_info "Creating crash-recovery VM overlay..."
    create_crash_overlay

    if run_vm; then
        log_info "Test PASSED: All crash recovery points verified"
        exit 0
    else
        log_error "Test FAILED: crash-recovery"
        log_info "Logs available at: $WORK_DIR"
        trap - EXIT  # Don't cleanup on failure
        exit 1
    fi
}

main "$@"
