# Makefile for fat32expander
#
# Targets:
#   make build       - Build debug binary
#   make release     - Build optimized release binary
#   make static      - Build static Linux x86_64 binary (musl)
#   make test        - Run unit tests
#   make test-qemu   - Run QEMU integration tests
#   make test-all    - Run all tests
#   make clean       - Clean build artifacts
#   make fmt         - Format code
#   make lint        - Run clippy linter
#   make check       - Run fmt check + clippy + tests
#   make ci-heavy    - Run the CI suite that needs dosfstools, mtools and QEMU
#   make ci-container - Run ci-heavy in the Containerfile image (podman/docker)
#   make demo        - Record docs/demo/fat32expander.webp (needs ansidrama)
#
# Cross-compilation targets:
#   make linux-x64   - Linux x86_64 (static musl)
#   make linux-arm64 - Linux ARM64 (static musl)
#   make macos-x64   - macOS x86_64 (requires macOS or cross toolchain)
#   make macos-arm64 - macOS ARM64 (requires macOS or cross toolchain)
#   make all-targets - Build all cross-compilation targets

CARGO := cargo
BINARY_NAME := fat32expander
# Where cargo writes its output: CARGO_TARGET_DIR when set, else target/
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

# Target triples
TARGET_LINUX_X64    := x86_64-unknown-linux-musl
TARGET_LINUX_ARM64  := aarch64-unknown-linux-musl
TARGET_MACOS_X64    := x86_64-apple-darwin
TARGET_MACOS_ARM64  := aarch64-apple-darwin

# Output directory for release binaries
DIST_DIR := dist

# `make man` writes man/fat32expander.8 from docs/manual.md
MAN_NAME = fat32expander
include build/man.mk

.PHONY: all build release static test test-unit test-qemu test-qemu-one test-all clean fmt fmt-check lint check help
.PHONY: info dry-run
.PHONY: ci-heavy ci-container demo
# No Windows target: the program uses Unix file APIs and does not build there.
.PHONY: linux-x64 linux-arm64 macos-x64 macos-arm64 all-targets dist setup-targets

# Default target
all: build

# Build debug binary
build:
	$(CARGO) build

# Build optimized release binary
release:
	$(CARGO) build --release

# Build static Linux x86_64 binary using musl (alias for linux-x64)
static: linux-x64

#
# Cross-compilation targets
#

# Linux x86_64 (static, musl)
linux-x64:
	$(CARGO) build --release --target $(TARGET_LINUX_X64)
	@echo "Built: $(TARGET_DIR)/$(TARGET_LINUX_X64)/release/$(BINARY_NAME)"

# Linux ARM64 (static, musl)
# Requires: rustup target add aarch64-unknown-linux-musl
#           Install cross-compiler: apt install gcc-aarch64-linux-gnu
linux-arm64:
	$(CARGO) build --release --target $(TARGET_LINUX_ARM64)
	@echo "Built: $(TARGET_DIR)/$(TARGET_LINUX_ARM64)/release/$(BINARY_NAME)"

# macOS x86_64
# Requires: rustup target add x86_64-apple-darwin
#           On Linux: Install osxcross or use cross-rs
#           On macOS: Works natively
macos-x64:
	$(CARGO) build --release --target $(TARGET_MACOS_X64)
	@echo "Built: $(TARGET_DIR)/$(TARGET_MACOS_X64)/release/$(BINARY_NAME)"

# macOS ARM64 (Apple Silicon)
# Requires: rustup target add aarch64-apple-darwin
#           On Linux: Install osxcross or use cross-rs
#           On macOS: Works natively
macos-arm64:
	$(CARGO) build --release --target $(TARGET_MACOS_ARM64)
	@echo "Built: $(TARGET_DIR)/$(TARGET_MACOS_ARM64)/release/$(BINARY_NAME)"

# Build all targets (will fail if cross-compilers not installed)
all-targets: linux-x64 linux-arm64 macos-x64 macos-arm64

# Install Rust targets (still need system cross-compilers)
setup-targets:
	rustup target add $(TARGET_LINUX_X64)
	rustup target add $(TARGET_LINUX_ARM64)
	rustup target add $(TARGET_MACOS_X64)
	rustup target add $(TARGET_MACOS_ARM64)
	@echo ""
	@echo "Rust targets installed. You also need system cross-compilers:"
	@echo "  Linux ARM64: apt install gcc-aarch64-linux-gnu"
	@echo "  macOS:       Use osxcross or build on macOS"

# Create distribution directory with all binaries
dist: linux-x64 linux-arm64
	@mkdir -p $(DIST_DIR)
	cp $(TARGET_DIR)/$(TARGET_LINUX_X64)/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-linux-x64
	cp $(TARGET_DIR)/$(TARGET_LINUX_ARM64)/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-linux-arm64
	@echo "Distribution binaries in $(DIST_DIR)/"
	@ls -la $(DIST_DIR)/

# Run unit tests
test-unit:
	$(CARGO) test

# Run QEMU integration tests
test-qemu: static
	./scripts/qemu/test-qemu.sh

# Run a specific QEMU test (usage: make test-qemu-one TEST=test-near-full)
test-qemu-one: static
	./scripts/qemu/test-qemu.sh --test $(TEST)

# Run all tests
test-all: test-unit test-qemu

# Alias for test-unit
test: test-unit

# Clean build artifacts
clean:
	$(CARGO) clean
	rm -rf "$${TMPDIR:-/tmp}/fat32expander-qemu"

# Format code
fmt:
	$(CARGO) fmt

# Check formatting without modifying
fmt-check:
	$(CARGO) fmt -- --check

# Run clippy linter
lint:
	$(CARGO) clippy -- -D warnings

# Full check: format, lint, and test
check: fmt-check lint test-unit

# The CI suite beyond what ri-ci-rust runs (fmt, clippy, plain cargo test).
# It needs dosfstools, mtools, musl-tools and QEMU; CI runs it inside the
# Containerfile image through `make ci-container`.
#
# tests/ci_workflow_test.rs and tests/manifest_test.rs read this recipe:
# - the ignored tests run as an --all-targets sweep, so a new tests/*.rs
#   file runs in CI the day it is written ([F-R61]);
# - durability_test and crash_recovery_test compile to zero tests without
#   their feature, so a sweep cannot cover them and each keeps its own line;
# - the library must build without the `cli` feature ([F-27]).
# test-crash-recovery.sh is not found by test-qemu.sh's glob of
# scripts/qemu/tests/, so it is called on its own line too.
ci-heavy:
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(CARGO) check --lib --no-default-features
	$(CARGO) test --all-targets -- --ignored
	$(CARGO) test --features device-journal --test durability_test -- --ignored
	$(CARGO) test --features fault-injection --test crash_recovery_test -- --ignored
	./scripts/test-resize.sh
	./scripts/qemu/setup-alpine.sh setup
	$(CARGO) build --release --target $(TARGET_LINUX_X64)
	./scripts/qemu/test-qemu.sh
	./scripts/qemu/test-crash-recovery.sh

# Run ci-heavy in the Containerfile image, as CI does
ci-container:
	./scripts/ci-container.sh ci-heavy

# Record the README demo, docs/demo/fat32expander.webp, from
# demo/fat32expander.toml. Needs ansidrama on PATH (or ANSIDRAMA=<path>),
# dosfstools and mtools. CI does not run it; commit the WebP by hand.
demo: release
	demo/record.sh "$(abspath $(TARGET_DIR))/release"

# Show filesystem info (usage: make info DEVICE=/path/to/device)
info: release
	$(TARGET_DIR)/release/$(BINARY_NAME) info $(DEVICE)

# Dry run resize (usage: make dry-run DEVICE=/path/to/device)
dry-run: release
	$(TARGET_DIR)/release/$(BINARY_NAME) resize --dry-run --verbose $(DEVICE)

# Help
help:
	@echo "fat32expander Makefile"
	@echo ""
	@echo "Build targets:"
	@echo "  make build        Build debug binary"
	@echo "  make release      Build optimized release binary"
	@echo "  make static       Build static Linux x86_64 binary (musl)"
	@echo ""
	@echo "Cross-compilation targets:"
	@echo "  make linux-x64    Linux x86_64 (static musl)"
	@echo "  make linux-arm64  Linux ARM64 (static musl)"
	@echo "  make macos-x64    macOS x86_64 (Intel)"
	@echo "  make macos-arm64  macOS ARM64 (Apple Silicon)"
	@echo "  make all-targets  Build all cross-compilation targets"
	@echo "  make dist         Build the Linux binaries into dist/"
	@echo "  make setup-targets  Install Rust cross-compilation targets"
	@echo ""
	@echo "Test targets:"
	@echo "  make test         Run unit tests"
	@echo "  make test-unit    Run unit tests"
	@echo "  make test-qemu    Run QEMU integration tests"
	@echo "  make test-all     Run all tests (unit + QEMU)"
	@echo "  make test-qemu-one TEST=<name>  Run specific QEMU test"
	@echo ""
	@echo "Code quality:"
	@echo "  make fmt          Format code"
	@echo "  make fmt-check    Check formatting"
	@echo "  make lint         Run clippy linter"
	@echo "  make check        Run fmt-check + lint + test-unit"
	@echo "  make ci-heavy     Run the CI suite that needs dosfstools, mtools, QEMU"
	@echo "  make ci-container Run ci-heavy in the Containerfile image"
	@echo ""
	@echo "Utilities:"
	@echo "  make info DEVICE=<path>     Show filesystem info"
	@echo "  make dry-run DEVICE=<path>  Dry run resize"
	@echo "  make demo                   Record the README demo (needs ansidrama)"
	@echo "  make clean                  Clean build artifacts"
	@echo ""
	@echo "Cross-compilation prerequisites:"
	@echo "  Linux ARM64: apt install gcc-aarch64-linux-gnu"
	@echo "  macOS:       Use osxcross or build on macOS natively"
	@echo ""
	@echo "Examples:"
	@echo "  make static && make test-qemu"
	@echo "  make test-qemu-one TEST=test-near-full"
	@echo "  make dist  # Build the Linux binaries into dist/"
