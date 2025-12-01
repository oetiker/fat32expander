#!/bin/bash
# Test: Resize with FAT table growth
#
# This test verifies resize functionality when the FAT tables need
# to grow, potentially requiring cluster relocation.

TEST_DESCRIPTION="Resize with FAT table growth and potential cluster relocation"
TEST_IMAGE_SIZE_MB=128
TEST_RESIZE_TO_MB=256
TEST_DATA_TYPE="simple"
