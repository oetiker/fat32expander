#!/bin/bash
# Test data generation for QEMU-based FAT32 testing
# These functions are designed to run inside the VM where we have real vfat support

# Generate deterministic content based on seed and size
generate_content() {
    local seed="$1"
    local size="$2"

    # Use seed to generate predictable content
    # This works in both host (for reference) and guest
    if [ "$size" -le 1024 ]; then
        # Small file: use text pattern
        printf "%s\n" "Content for seed $seed" | head -c "$size"
    else
        # Larger file: use /dev/urandom with seed (via openssl for consistency)
        dd if=/dev/urandom bs="$size" count=1 2>/dev/null | \
            openssl enc -aes-256-ctr -nosalt -pass pass:"$seed" 2>/dev/null | \
            head -c "$size"
    fi
}

# Create a simple test file
create_test_file() {
    local mount_point="$1"
    local filename="$2"
    local size="${3:-100}"
    local seed="${4:-default}"

    local filepath="$mount_point/$filename"
    local dirpath=$(dirname "$filepath")

    # Ensure directory exists
    mkdir -p "$dirpath" 2>/dev/null || true

    # Generate content
    generate_content "$seed-$filename" "$size" > "$filepath"
}

# Create files with funky (Unicode, LFN, special char) names
create_funky_files() {
    local mount_point="$1"
    local seed="${2:-funky}"

    echo "Creating funky filename test files..."

    # Long filenames (LFN > 8.3)
    create_test_file "$mount_point" "This is a very long filename that exceeds eight point three.txt" 100 "$seed"
    create_test_file "$mount_point" "Another Long Filename With Spaces.doc" 200 "$seed"
    create_test_file "$mount_point" "multiple.dots.in.filename.test.txt" 150 "$seed"

    # Unicode/UTF-8 filenames
    create_test_file "$mount_point" "café_résumé.txt" 100 "$seed"
    create_test_file "$mount_point" "Größe_Übung.txt" 100 "$seed"
    create_test_file "$mount_point" "日本語ファイル.txt" 100 "$seed"
    create_test_file "$mount_point" "файл_на_русском.txt" 100 "$seed"
    create_test_file "$mount_point" "ελληνικά.txt" 100 "$seed"
    create_test_file "$mount_point" "עברית.txt" 100 "$seed"

    # Special characters (FAT32-safe subset)
    create_test_file "$mount_point" "file-with-dashes.txt" 100 "$seed"
    create_test_file "$mount_point" "file_with_underscores.txt" 100 "$seed"
    create_test_file "$mount_point" "UPPERCASE.TXT" 100 "$seed"
    create_test_file "$mount_point" "lowercase.txt" 100 "$seed"
    create_test_file "$mount_point" "MixedCaseFile.Txt" 100 "$seed"

    # Edge cases
    create_test_file "$mount_point" "a.txt" 50 "$seed"
    create_test_file "$mount_point" "ab.txt" 50 "$seed"
    create_test_file "$mount_point" "abcdefgh.txt" 50 "$seed"  # Exact 8.3

    # Numbers and combinations
    create_test_file "$mount_point" "file123.txt" 100 "$seed"
    create_test_file "$mount_point" "123file.txt" 100 "$seed"
    create_test_file "$mount_point" "2024-11-28_report.txt" 100 "$seed"

    echo "Created funky filename test files"
}

# Create a deep directory hierarchy
create_deep_hierarchy() {
    local mount_point="$1"
    local depth="${2:-12}"
    local seed="${3:-deep}"

    echo "Creating ${depth}-level deep hierarchy..."

    local current_path="$mount_point"

    for i in $(seq 1 "$depth"); do
        current_path="$current_path/level_$i"
        mkdir -p "$current_path"

        # Add a file at each level
        create_test_file "$current_path" "file_at_level_$i.txt" 100 "$seed-$i"

        # Add a larger binary file at some levels
        if [ $((i % 3)) -eq 0 ]; then
            create_test_file "$current_path" "binary_$i.bin" 4096 "$seed-bin-$i"
        fi
    done

    # Add extra file at deepest level
    create_test_file "$current_path" "deepest_file.txt" 500 "$seed-deepest"
    create_test_file "$current_path" "deepest_binary.bin" 8192 "$seed-deepest-bin"

    echo "Created ${depth}-level hierarchy"
}

# Create simple test files (for basic resize tests)
create_simple_files() {
    local mount_point="$1"
    local seed="${2:-simple}"

    echo "Creating simple test files..."

    create_test_file "$mount_point" "hello.txt" 12 "$seed"
    echo "Hello World" > "$mount_point/hello.txt"  # Override with known content

    create_test_file "$mount_point" "test1.txt" 100 "$seed"
    create_test_file "$mount_point" "test2.txt" 200 "$seed"
    create_test_file "$mount_point" "random.bin" 102400 "$seed"  # 100KB

    # Create a subdirectory with files
    mkdir -p "$mount_point/subdir"
    create_test_file "$mount_point/subdir/nested.txt" 150 "$seed"
    create_test_file "$mount_point/subdir/data.bin" 51200 "$seed"  # 50KB

    echo "Created simple test files"
}

# Fill filesystem to near capacity
create_fill_files() {
    local mount_point="$1"
    local target_percent="${2:-90}"
    local seed="${3:-fill}"

    echo "Filling filesystem to ~${target_percent}%..."

    # Get filesystem size info
    local fs_info=$(df -B1 "$mount_point" | tail -1)
    local total=$(echo "$fs_info" | awk '{print $2}')
    local used=$(echo "$fs_info" | awk '{print $3}')
    local target_bytes=$((total * target_percent / 100))
    local to_write=$((target_bytes - used))

    if [ "$to_write" -le 0 ]; then
        echo "Filesystem already at or above ${target_percent}%"
        return 0
    fi

    echo "Need to write approximately $((to_write / 1024))KB"

    # Write files in chunks
    local file_size=51200  # 50KB per file
    local file_num=1

    while [ "$to_write" -gt "$file_size" ]; do
        create_test_file "$mount_point" "fill_$(printf '%04d' $file_num).bin" "$file_size" "$seed-$file_num"
        to_write=$((to_write - file_size))
        file_num=$((file_num + 1))

        # Progress every 20 files
        if [ $((file_num % 20)) -eq 0 ]; then
            echo "  Written $file_num files..."
        fi
    done

    # Write remaining
    if [ "$to_write" -gt 0 ]; then
        create_test_file "$mount_point" "fill_final.bin" "$to_write" "$seed-final"
    fi

    echo "Created $file_num fill files"
}

# Generate checksums for all files in a directory
generate_checksums() {
    local mount_point="$1"
    local output_file="$2"

    echo "Generating checksums..."

    # Find all files and generate SHA256 checksums
    # Store relative paths for portability
    (cd "$mount_point" && find . -type f -exec sha256sum {} \;) | sort > "$output_file"

    local count=$(wc -l < "$output_file")
    echo "Generated checksums for $count files"
}

# Verify checksums
verify_checksums() {
    local mount_point="$1"
    local checksum_file="$2"

    echo "Verifying checksums..."

    if [ ! -f "$checksum_file" ]; then
        echo "ERROR: Checksum file not found: $checksum_file"
        return 1
    fi

    # Verify from mount point directory
    (cd "$mount_point" && sha256sum -c "$checksum_file")
    local result=$?

    if [ $result -eq 0 ]; then
        echo "All checksums verified successfully"
    else
        echo "ERROR: Checksum verification failed"
    fi

    return $result
}

# List all files with their sizes (for debugging)
list_files() {
    local mount_point="$1"

    echo "Files in $mount_point:"
    find "$mount_point" -type f -exec ls -la {} \; | sort
    echo ""
    echo "Directories:"
    find "$mount_point" -type d | sort
}
