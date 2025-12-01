#!/bin/bash
#
# QEMU-based FAT32 testing for fat32expander
#
# Tests fat32expander with real Linux vfat kernel drivers by running
# everything inside a VM:
# 1. Create FAT32 image in tmpfs
# 2. Populate with test data
# 3. Resize with fat32expander
# 4. Verify checksums
#
# Usage:
#   ./test-qemu.sh                    # Run all tests
#   ./test-qemu.sh --test test-name   # Run specific test
#   ./test-qemu.sh --list             # List available tests
#

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$(dirname "$SCRIPT_DIR")")"

# Source common utilities
source "$SCRIPT_DIR/lib/common.sh"

# Configuration
WORK_BASE="${TMPDIR:-/tmp}/fat32expander-qemu"
KEEP_ALL=false
SPECIFIC_TEST=""
VM_TIMEOUT=120

# Parse arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --test)
            SPECIFIC_TEST="$2"
            shift 2
            ;;
        --keep-all)
            KEEP_ALL=true
            shift
            ;;
        --timeout)
            VM_TIMEOUT="$2"
            shift 2
            ;;
        --list)
            echo "Available tests:"
            for t in "$SCRIPT_DIR/tests"/test-*.sh; do
                basename "$t" .sh
            done
            exit 0
            ;;
        --help|-h)
            echo "Usage: $0 [options]"
            echo ""
            echo "Options:"
            echo "  --test <name>     Run specific test only"
            echo "  --keep-all        Keep work directories even on success"
            echo "  --timeout <sec>   VM timeout in seconds (default: 120)"
            echo "  --list            List available tests"
            echo "  --help            Show this help"
            exit 0
            ;;
        *)
            log_error "Unknown option: $1"
            exit 1
            ;;
    esac
done

# Ensure Alpine kernel/initrd are available
ensure_alpine() {
    log_info "Checking Alpine setup..."
    if ! "$SCRIPT_DIR/setup-alpine.sh" check >/dev/null 2>&1; then
        log_info "Setting up Alpine Linux..."
        "$SCRIPT_DIR/setup-alpine.sh" setup
    fi
    eval $("$SCRIPT_DIR/setup-alpine.sh" paths)
    export ALPINE_KERNEL ALPINE_INITRD
    log_info "Using kernel: $ALPINE_KERNEL"
}

# Ensure fat32expander binary exists
ensure_binary() {
    BINARY=$(get_binary_path "$SCRIPT_DIR")
    if [ -z "$BINARY" ] || [ ! -x "$BINARY" ]; then
        log_error "Failed to find/build fat32expander binary"
        exit 1
    fi
    export BINARY
    log_info "Using binary: $BINARY"
}

# Create test data script based on data type
# This script runs inside the VM at /scripts/test.sh
create_test_script() {
    local output_file="$1"
    local data_type="$2"
    local extra_args="$3"

    cat > "$output_file" << 'SCRIPT_HEADER'
#!/bin/sh
# Test data script - runs inside VM
# Usage: test.sh populate <mount_point>

ACTION="$1"
MOUNT_POINT="$2"

if [ "$ACTION" != "populate" ]; then
    echo "Unknown action: $ACTION"
    exit 1
fi

cd "$MOUNT_POINT" || exit 1
echo "Populating filesystem at $MOUNT_POINT..."
SCRIPT_HEADER

    case "$data_type" in
        simple)
            cat >> "$output_file" << 'SIMPLE_DATA'
echo "Creating simple test files..."
echo "Hello World" > hello.txt
echo "Test file 1" > test1.txt
echo "Test file 2" > test2.txt
dd if=/dev/urandom of=random.bin bs=1024 count=100 2>/dev/null
mkdir -p subdir
echo "Nested file" > subdir/nested.txt
dd if=/dev/urandom of=subdir/data.bin bs=1024 count=50 2>/dev/null
echo "Created simple test files"
SIMPLE_DATA
            ;;
        funky)
            cat >> "$output_file" << 'FUNKY_DATA'
echo "Creating funky filename test files..."

# Long filenames (LFN)
echo "LFN content" > "This is a very long filename that exceeds eight point three.txt"
echo "Another LFN" > "Another Long Filename With Spaces.doc"
echo "Dots" > "multiple.dots.in.filename.test.txt"

# Unicode filenames (if supported by mount options)
echo "French" > "cafe_resume.txt"
echo "German" > "Grosse_Uebung.txt"

# Special characters (safe subset)
echo "dashes" > "file-with-dashes.txt"
echo "underscores" > "file_with_underscores.txt"
echo "upper" > "UPPERCASE.TXT"
echo "lower" > "lowercase.txt"
echo "mixed" > "MixedCaseFile.Txt"

# Edge cases
echo "a" > "a.txt"
echo "ab" > "ab.txt"
echo "exact83" > "abcdefgh.txt"

# Binary data
dd if=/dev/urandom of=funky_data.bin bs=1024 count=100 2>/dev/null

echo "Created funky filename test files"
FUNKY_DATA
            ;;
        deep)
            local depth="${extra_args:-12}"
            cat >> "$output_file" << DEEP_DATA
echo "Creating deep directory hierarchy..."
DEPTH=$depth
CURRENT="."
i=1
while [ \$i -le \$DEPTH ]; do
    CURRENT="\$CURRENT/level_\$i"
    mkdir -p "\$CURRENT"
    echo "File at level \$i" > "\$CURRENT/file_at_level_\$i.txt"
    if [ \$((i % 3)) -eq 0 ]; then
        dd if=/dev/urandom of="\$CURRENT/binary_\$i.bin" bs=1024 count=4 2>/dev/null
    fi
    i=\$((i + 1))
done
echo "Deepest file" > "\$CURRENT/deepest_file.txt"
dd if=/dev/urandom of="\$CURRENT/deepest_binary.bin" bs=1024 count=8 2>/dev/null
echo "Root file" > root.txt
echo "Created ${depth}-level hierarchy"
DEEP_DATA
            ;;
        largedir)
            cat >> "$output_file" << 'LARGEDIR_DATA'
echo "Creating large directory with many files..."

# Create a directory with 500+ files to force it to span multiple clusters
# With 512-byte clusters and 32-byte directory entries = 16 entries/cluster
# LFN entries use ~3 entries per file, so ~5 files per cluster
# 500 files = ~100 clusters of directory data

mkdir -p bigdir
for i in $(seq 1 500); do
    # Use long filenames to consume more directory entries
    echo "Content of file number $i" > "bigdir/file_with_long_name_number_$(printf '%04d' $i).txt"
    if [ $((i % 100)) -eq 0 ]; then
        echo "  Created $i/500 files..."
    fi
done

# Also create some files in root to ensure root directory works
echo "Root level file" > root_file.txt
dd if=/dev/urandom of=root_binary.bin bs=1024 count=50 2>/dev/null

# Nested large directory
mkdir -p nested/subbigdir
for i in $(seq 1 200); do
    echo "Nested content $i" > "nested/subbigdir/nested_file_$(printf '%03d' $i).txt"
done

echo "Created 500 files in bigdir, 200 in nested/subbigdir"
LARGEDIR_DATA
            ;;
        fragmented)
            cat >> "$output_file" << 'FRAGMENTED_DATA'
echo "Creating fragmented files..."

# Create 5 files that we'll extend in rotation to cause fragmentation
NUM_FILES=5
NUM_ROUNDS=50
CHUNK_SIZE=10  # KB per extension

# Initialize files with unique headers
for i in $(seq 1 $NUM_FILES); do
    echo "=== FILE $i HEADER ===" > "fragfile_$i.bin"
done

# Extend files in rotation to fragment them
# Each round: file1 grows, file2 grows, file3 grows, ...
# This interleaves their clusters on disk
for round in $(seq 1 $NUM_ROUNDS); do
    for i in $(seq 1 $NUM_FILES); do
        # Append random data to each file
        dd if=/dev/urandom bs=1024 count=$CHUNK_SIZE 2>/dev/null >> "fragfile_$i.bin"
    done
    if [ $((round % 10)) -eq 0 ]; then
        echo "  Round $round/$NUM_ROUNDS complete..."
    fi
done

# Add unique footers so we can verify file integrity
for i in $(seq 1 $NUM_FILES); do
    echo "=== FILE $i FOOTER ===" >> "fragfile_$i.bin"
done

# Also create some normal files to verify mixed scenarios work
echo "Normal file content" > normal.txt
dd if=/dev/urandom of=normal_binary.bin bs=1024 count=100 2>/dev/null

# Create a directory with fragmented files too
mkdir -p fragdir
for i in $(seq 1 3); do
    echo "=== SUBFILE $i ===" > "fragdir/subfile_$i.bin"
    for round in $(seq 1 20); do
        dd if=/dev/urandom bs=1024 count=5 2>/dev/null >> "fragdir/subfile_$i.bin"
    done
done

echo "Created $NUM_FILES fragmented files with $NUM_ROUNDS rounds of $CHUNK_SIZE KB each"
FRAGMENTED_DATA
            ;;
        fill)
            local percent="${extra_args:-80}"
            cat >> "$output_file" << FILL_DATA
echo "Filling filesystem to ~${percent}%..."
echo "Structured file" > structured.txt
mkdir -p filldir
FILE_NUM=1
while true; do
    # Check disk usage
    USED=\$(df "\$MOUNT_POINT" | tail -1 | awk '{print \$5}' | tr -d '%')
    if [ "\$USED" -ge ${percent} ]; then
        break
    fi
    dd if=/dev/urandom of="filldir/fill_\$FILE_NUM.bin" bs=1024 count=50 2>/dev/null || break
    FILE_NUM=\$((FILE_NUM + 1))
    if [ \$((FILE_NUM % 20)) -eq 0 ]; then
        echo "  Written \$FILE_NUM files, \${USED}% used..."
    fi
done
echo "Created \$FILE_NUM fill files"
FILL_DATA
            ;;
    esac

    cat >> "$output_file" << 'SCRIPT_FOOTER'

sync
echo "Population complete"
SCRIPT_FOOTER

    chmod +x "$output_file"
}

# Run a single test
run_test() {
    local test_script="$1"
    local test_name=$(basename "$test_script" .sh)
    local work_dir="$WORK_BASE/$test_name-$$"
    local result=0

    log_test "$test_name"
    mkdir -p "$work_dir"

    # Source test configuration
    TEST_IMAGE_SIZE_MB=128
    TEST_RESIZE_TO_MB=256
    TEST_DATA_TYPE="simple"
    TEST_DESCRIPTION="No description"
    TEST_EXTRA_ARGS=""
    TEST_SECTOR_SIZE=512
    source "$test_script"

    log_info "Description: $TEST_DESCRIPTION"
    log_info "Image: ${TEST_IMAGE_SIZE_MB}MB -> ${TEST_RESIZE_TO_MB}MB"
    log_info "Sector size: ${TEST_SECTOR_SIZE} bytes"
    log_info "Data type: $TEST_DATA_TYPE"

    # Create test data script
    local test_data_script="$work_dir/test-data.sh"
    create_test_script "$test_data_script" "$TEST_DATA_TYPE" "$TEST_EXTRA_ARGS"

    # Run the VM test
    if ! "$SCRIPT_DIR/run-vm.sh" \
            "$work_dir" \
            "$test_data_script" \
            "$BINARY" \
            "$ALPINE_KERNEL" \
            "$ALPINE_INITRD" \
            "$VM_TIMEOUT" \
            "$TEST_IMAGE_SIZE_MB" \
            "$TEST_RESIZE_TO_MB" \
            "$TEST_SECTOR_SIZE"; then
        log_error "Test FAILED: $test_name"
        result=1
    else
        log_info "Test PASSED: $test_name"
    fi

    # Cleanup or preserve
    if [ $result -eq 0 ] && [ "$KEEP_ALL" != "true" ]; then
        rm -rf "$work_dir"
    else
        log_info "Work dir: $work_dir"
    fi

    return $result
}

# Main
main() {
    echo "=============================================="
    echo "fat32expander QEMU Test Suite"
    echo "=============================================="

    if ! check_qemu_prerequisites; then
        log_error "Prerequisites not met"
        exit 1
    fi

    ensure_alpine
    ensure_binary
    mkdir -p "$WORK_BASE"

    # Find tests
    local tests=()
    if [ -n "$SPECIFIC_TEST" ]; then
        local test_file="$SCRIPT_DIR/tests/$SPECIFIC_TEST.sh"
        [ ! -f "$test_file" ] && test_file="$SCRIPT_DIR/tests/test-$SPECIFIC_TEST.sh"
        if [ ! -f "$test_file" ]; then
            log_error "Test not found: $SPECIFIC_TEST"
            exit 1
        fi
        tests=("$test_file")
    else
        mapfile -t tests < <(find "$SCRIPT_DIR/tests" -name "test-*.sh" -type f | sort)
    fi

    [ ${#tests[@]} -eq 0 ] && { log_error "No tests found"; exit 1; }
    log_info "Found ${#tests[@]} test(s)"

    local passed=0 failed=0
    for test_script in "${tests[@]}"; do
        if run_test "$test_script"; then
            ((++passed))
        else
            ((++failed))
        fi
    done

    echo ""
    echo "=============================================="
    echo -e "Results: ${GREEN}$passed passed${NC}, ${RED}$failed failed${NC}"
    echo "=============================================="

    [ $failed -gt 0 ] && exit 1
    exit 0
}

main "$@"
