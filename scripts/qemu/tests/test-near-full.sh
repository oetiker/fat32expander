#!/bin/bash
# Test: Full filesystem resize
#
# This test fills the filesystem to 100% capacity before resizing
# to verify that resize works correctly when there's NO free space.
# The resizer must use the newly allocated space for relocation targets.

TEST_DESCRIPTION="Resize with filesystem at 100% capacity (full)"
TEST_IMAGE_SIZE_MB=64
TEST_RESIZE_TO_MB=128
TEST_DATA_TYPE="fill"
TEST_EXTRA_ARGS="100"
