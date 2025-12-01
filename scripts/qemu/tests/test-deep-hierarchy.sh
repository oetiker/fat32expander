#!/bin/bash
# Test: Deep directory hierarchy
#
# This test creates a 12-level deep directory structure with files
# at each level to verify that directory entry updates work correctly
# during resize with FAT growth.

TEST_DESCRIPTION="12-level deep directory hierarchy"
TEST_IMAGE_SIZE_MB=128
TEST_RESIZE_TO_MB=512
TEST_DATA_TYPE="deep"
TEST_DEEP_LEVELS=12
