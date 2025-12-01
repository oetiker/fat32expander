#!/bin/bash
# Test script for fat32expander
# This script creates test FAT32 images and verifies the resize functionality

set -e

# Ensure sbin directories are in PATH (dosfstools often installed there)
export PATH="$PATH:/sbin:/usr/sbin"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
WORK_DIR="${TMPDIR:-/tmp}/fat32expander-test-$$"
BINARY="$PROJECT_DIR/target/release/fat32expander"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

cleanup() {
    echo "Cleaning up..."
    rm -rf "$WORK_DIR"
}
trap cleanup EXIT

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
    echo -e "${YELLOW}========================================${NC}"
    echo -e "${YELLOW}TEST: $1${NC}"
    echo -e "${YELLOW}========================================${NC}"
}

# Check prerequisites
check_prerequisites() {
    log_info "Checking prerequisites..."

    if ! command -v mkfs.fat &> /dev/null; then
        log_error "mkfs.fat not found. Please install dosfstools."
        exit 1
    fi

    if ! command -v dosfsck &> /dev/null; then
        log_error "dosfsck not found. Please install dosfstools."
        exit 1
    fi

    # Check for mtools (optional but recommended)
    if command -v mcopy &> /dev/null; then
        HAVE_MTOOLS=1
        log_info "mtools found - will run file content tests"
    else
        HAVE_MTOOLS=0
        log_warn "mtools not found - skipping file content tests"
        log_warn "Install mtools package for comprehensive testing"
    fi

    if [ ! -f "$BINARY" ]; then
        log_info "Building release binary..."
        cd "$PROJECT_DIR"
        cargo build --release
    fi
}

# Create a test image
create_image() {
    local path="$1"
    local size_mb="$2"

    log_info "Creating ${size_mb}MB FAT32 image at $path"
    truncate -s "${size_mb}M" "$path"
    mkfs.fat -F 32 "$path" > /dev/null 2>&1
}

# Extend an image
extend_image() {
    local path="$1"
    local new_size_mb="$2"

    log_info "Extending image to ${new_size_mb}MB"
    truncate -s "${new_size_mb}M" "$path"
}

# Check filesystem
check_fs() {
    local path="$1"
    local desc="$2"

    log_info "Checking filesystem ($desc)..."
    if dosfsck -n "$path" 2>&1; then
        log_info "Filesystem check passed"
        return 0
    else
        log_error "Filesystem check failed"
        return 1
    fi
}

# Mount image and add files using mtools (doesn't require root)
add_test_files() {
    local path="$1"
    local pattern="$2"  # simple, nested, or deep

    log_info "Adding test files to image (pattern: $pattern)..."

    case "$pattern" in
        simple)
            # Add a few files to the root directory
            echo "Hello World" | mcopy -i "$path" - ::hello.txt
            echo "Test file 1" | mcopy -i "$path" - ::test1.txt
            echo "Test file 2" | mcopy -i "$path" - ::test2.txt
            dd if=/dev/urandom bs=1024 count=100 2>/dev/null | mcopy -i "$path" - ::random.bin
            ;;
        nested)
            # Create a directory structure
            mmd -i "$path" ::subdir1
            mmd -i "$path" ::subdir2
            echo "File in subdir1" | mcopy -i "$path" - ::subdir1/file1.txt
            echo "File in subdir2" | mcopy -i "$path" - ::subdir2/file2.txt
            dd if=/dev/urandom bs=1024 count=50 2>/dev/null | mcopy -i "$path" - ::subdir1/data.bin
            ;;
        deep)
            # Create deep directory hierarchy
            mmd -i "$path" ::level1
            mmd -i "$path" ::level1/level2
            mmd -i "$path" ::level1/level2/level3
            mmd -i "$path" ::level1/level2/level3/level4
            echo "Root file" | mcopy -i "$path" - ::root.txt
            echo "Level 1 file" | mcopy -i "$path" - ::level1/l1.txt
            echo "Level 2 file" | mcopy -i "$path" - ::level1/level2/l2.txt
            echo "Level 3 file" | mcopy -i "$path" - ::level1/level2/level3/l3.txt
            echo "Level 4 file" | mcopy -i "$path" - ::level1/level2/level3/level4/l4.txt
            # Add some larger files
            dd if=/dev/urandom bs=1024 count=100 2>/dev/null | mcopy -i "$path" - ::level1/level2/data.bin
            ;;
    esac
}

# Verify files after resize
verify_files() {
    local path="$1"
    local pattern="$2"

    log_info "Verifying files after resize (pattern: $pattern)..."

    # Note: mdir displays FAT32 8.3 filenames with spaces, e.g., "hello    txt"
    # Use case-insensitive grep (-i) on the base filename
    case "$pattern" in
        simple)
            mdir -i "$path" :: | grep -iq "hello" || { log_error "hello.txt missing!"; return 1; }
            mdir -i "$path" :: | grep -iq "test1" || { log_error "test1.txt missing!"; return 1; }
            mdir -i "$path" :: | grep -iq "random" || { log_error "random.bin missing!"; return 1; }
            # Verify content
            local content
            content=$(mcopy -i "$path" ::hello.txt - 2>/dev/null)
            if [ "$content" != "Hello World" ]; then
                log_error "hello.txt content mismatch!"
                return 1
            fi
            ;;
        nested)
            mdir -i "$path" ::subdir1 | grep -iq "file1" || { log_error "subdir1/file1.txt missing!"; return 1; }
            mdir -i "$path" ::subdir2 | grep -iq "file2" || { log_error "subdir2/file2.txt missing!"; return 1; }
            # Verify content
            local content
            content=$(mcopy -i "$path" ::subdir1/file1.txt - 2>/dev/null)
            if [ "$content" != "File in subdir1" ]; then
                log_error "subdir1/file1.txt content mismatch!"
                return 1
            fi
            ;;
        deep)
            mdir -i "$path" :: | grep -iq "level1" || { log_error "level1 directory missing!"; return 1; }
            mdir -i "$path" ::level1/level2/level3/level4 | grep -iq "l4" || { log_error "Deep file l4.txt missing!"; return 1; }
            # Verify content
            local content
            content=$(mcopy -i "$path" ::level1/level2/level3/level4/l4.txt - 2>/dev/null)
            if [ "$content" != "Level 4 file" ]; then
                log_error "l4.txt content mismatch!"
                return 1
            fi
            ;;
    esac

    log_info "All files verified successfully"
    return 0
}

# Show filesystem info
show_info() {
    local path="$1"
    "$BINARY" info "$path"
}

# Run resize
run_resize() {
    local path="$1"
    shift
    "$BINARY" resize "$@" "$path"
}

# Test 1: Info command
test_info_command() {
    log_test "Info Command"

    local img="$WORK_DIR/test_info.img"
    create_image "$img" 128

    log_info "Running info command..."
    show_info "$img"

    log_info "Test passed!"
}

# Test 2: Simple resize (no FAT growth)
test_simple_resize() {
    log_test "Simple Resize (No FAT Growth)"

    local img="$WORK_DIR/test_simple.img"
    create_image "$img" 128
    extend_image "$img" 140

    log_info "Before resize:"
    show_info "$img" | grep -E "(Total sectors|FAT size|Can grow)"

    check_fs "$img" "before resize"

    log_info "Running resize..."
    run_resize "$img" --verbose --force

    log_info "After resize:"
    show_info "$img" | grep -E "(Total sectors|FAT size|Can grow)"

    check_fs "$img" "after resize"

    log_info "Test passed!"
}

# Test 3: Resize with FAT growth
test_fat_growth_resize() {
    log_test "Resize with FAT Growth"

    local img="$WORK_DIR/test_fat_growth.img"
    create_image "$img" 128
    extend_image "$img" 256

    log_info "Before resize:"
    show_info "$img" | grep -E "(Total sectors|FAT size|Can grow|Root directory cluster)"

    check_fs "$img" "before resize"

    log_info "Running resize..."
    run_resize "$img" --verbose --force

    log_info "After resize:"
    show_info "$img" | grep -E "(Total sectors|FAT size|Can grow|Root directory cluster)"

    check_fs "$img" "after resize"

    log_info "Test passed!"
}

# Test 4: Dry run
test_dry_run() {
    log_test "Dry Run Mode"

    local img="$WORK_DIR/test_dry_run.img"
    create_image "$img" 128
    extend_image "$img" 256

    local before_sectors
    before_sectors=$(show_info "$img" | grep "Total sectors" | awk '{print $NF}')

    log_info "Running dry run..."
    run_resize "$img" --dry-run --verbose --force

    local after_sectors
    after_sectors=$(show_info "$img" | grep "Total sectors" | awk '{print $NF}')

    if [ "$before_sectors" = "$after_sectors" ]; then
        log_info "Dry run made no changes (as expected)"
        log_info "Test passed!"
    else
        log_error "Dry run modified the filesystem!"
        exit 1
    fi
}

# Test 5: Already at max size
test_already_max_size() {
    log_test "Already at Max Size"

    local img="$WORK_DIR/test_max_size.img"
    create_image "$img" 128
    # Don't extend - already at max

    log_info "Running resize (should fail)..."
    if run_resize "$img" --force 2>&1; then
        log_error "Resize should have failed but didn't"
        exit 1
    else
        log_info "Resize correctly failed (filesystem already at max size)"
        log_info "Test passed!"
    fi
}

# Test 6: Large resize (FAT must grow significantly)
test_large_resize() {
    log_test "Large Resize (Significant FAT Growth)"

    local img="$WORK_DIR/test_large.img"
    create_image "$img" 128
    extend_image "$img" 512

    log_info "Before resize:"
    show_info "$img" | grep -E "(Total sectors|FAT size|Data clusters)"

    check_fs "$img" "before resize"

    log_info "Running resize..."
    run_resize "$img" --verbose --force

    log_info "After resize:"
    show_info "$img" | grep -E "(Total sectors|FAT size|Data clusters)"

    check_fs "$img" "after resize"

    log_info "Test passed!"
}

# Test 7: Resize with simple files
test_resize_with_simple_files() {
    if [ "$HAVE_MTOOLS" -ne 1 ]; then
        log_warn "Skipping test (mtools not available)"
        return 0
    fi

    log_test "Resize with Simple Files"

    local img="$WORK_DIR/test_simple_files.img"
    create_image "$img" 128

    add_test_files "$img" simple

    log_info "Files before resize:"
    mdir -i "$img" ::

    extend_image "$img" 256

    check_fs "$img" "before resize"

    log_info "Running resize..."
    run_resize "$img" --verbose --force

    check_fs "$img" "after resize"

    verify_files "$img" simple

    log_info "Test passed!"
}

# Test 8: Resize with nested directories
test_resize_with_nested_dirs() {
    if [ "$HAVE_MTOOLS" -ne 1 ]; then
        log_warn "Skipping test (mtools not available)"
        return 0
    fi

    log_test "Resize with Nested Directories"

    local img="$WORK_DIR/test_nested.img"
    create_image "$img" 128

    add_test_files "$img" nested

    log_info "Directory structure before resize:"
    mdir -i "$img" ::
    mdir -i "$img" ::subdir1
    mdir -i "$img" ::subdir2

    extend_image "$img" 256

    check_fs "$img" "before resize"

    log_info "Running resize..."
    run_resize "$img" --verbose --force

    check_fs "$img" "after resize"

    verify_files "$img" nested

    log_info "Test passed!"
}

# Test 9: Resize with deep directory hierarchy
test_resize_with_deep_hierarchy() {
    if [ "$HAVE_MTOOLS" -ne 1 ]; then
        log_warn "Skipping test (mtools not available)"
        return 0
    fi

    log_test "Resize with Deep Directory Hierarchy"

    local img="$WORK_DIR/test_deep.img"
    create_image "$img" 128

    add_test_files "$img" deep

    log_info "Deep directory structure before resize:"
    mdir -i "$img" ::
    mdir -i "$img" ::level1
    mdir -i "$img" ::level1/level2
    mdir -i "$img" ::level1/level2/level3 2>/dev/null || true
    mdir -i "$img" ::level1/level2/level3/level4 2>/dev/null || true

    extend_image "$img" 512

    check_fs "$img" "before resize"

    log_info "Running resize (large growth with deep directories)..."
    run_resize "$img" --verbose --force

    check_fs "$img" "after resize"

    verify_files "$img" deep

    log_info "Test passed!"
}

# Main
main() {
    echo "=============================================="
    echo "fat32expander Test Suite"
    echo "=============================================="

    check_prerequisites

    mkdir -p "$WORK_DIR"

    test_info_command
    test_dry_run
    test_already_max_size
    test_simple_resize
    test_fat_growth_resize
    test_large_resize
    test_resize_with_simple_files
    test_resize_with_nested_dirs
    test_resize_with_deep_hierarchy

    echo ""
    echo -e "${GREEN}=============================================="
    echo "All tests passed!"
    echo "==============================================${NC}"
}

main "$@"
