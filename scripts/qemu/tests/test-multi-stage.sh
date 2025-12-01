#!/bin/bash
# Test: Multi-stage resize with ongoing filesystem activity
#
# This test simulates real-world usage where a filesystem is expanded
# multiple times with file activity between expansions:
# 1. Create filesystem with initial files
# 2. First resize
# 3. Add new files and append to existing files
# 4. Second resize
# 5. More file activity
# 6. Final verification
#
# This tests that:
# - Multiple resize operations work correctly
# - Files created after resize work properly
# - Appending to existing files (extending cluster chains) works after resize

TEST_DESCRIPTION="Multi-stage resize with file activity between expansions"
TEST_IMAGE_SIZE_MB=64
TEST_RESIZE_TO_MB=256  # Final size (will resize in stages: 64->128->256)
TEST_DATA_TYPE="multistage"
TEST_TIMEOUT=300  # Longer timeout for multiple operations
