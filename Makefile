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
#
# Cross-compilation targets:
#   make linux-x64   - Linux x86_64 (static musl)
#   make linux-arm64 - Linux ARM64 (static musl)
#   make windows     - Windows x86_64
#   make macos-x64   - macOS x86_64 (requires macOS or cross toolchain)
#   make macos-arm64 - macOS ARM64 (requires macOS or cross toolchain)
#   make all-targets - Build all cross-compilation targets

CARGO := cargo
BINARY_NAME := fat32expander

# Target triples
TARGET_LINUX_X64    := x86_64-unknown-linux-musl
TARGET_LINUX_ARM64  := aarch64-unknown-linux-musl
TARGET_WINDOWS_X64  := x86_64-pc-windows-gnu
TARGET_MACOS_X64    := x86_64-apple-darwin
TARGET_MACOS_ARM64  := aarch64-apple-darwin

# Output directory for release binaries
DIST_DIR := dist

.PHONY: all build release static test test-unit test-qemu test-all clean fmt lint check help
.PHONY: linux-x64 linux-arm64 windows macos-x64 macos-arm64 all-targets dist setup-targets

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
	@echo "Built: target/$(TARGET_LINUX_X64)/release/$(BINARY_NAME)"

# Linux ARM64 (static, musl)
# Requires: rustup target add aarch64-unknown-linux-musl
#           Install cross-compiler: apt install gcc-aarch64-linux-gnu
linux-arm64:
	$(CARGO) build --release --target $(TARGET_LINUX_ARM64)
	@echo "Built: target/$(TARGET_LINUX_ARM64)/release/$(BINARY_NAME)"

# Windows x86_64
# Requires: rustup target add x86_64-pc-windows-gnu
#           Install cross-compiler: apt install gcc-mingw-w64-x86-64
windows:
	$(CARGO) build --release --target $(TARGET_WINDOWS_X64)
	@echo "Built: target/$(TARGET_WINDOWS_X64)/release/$(BINARY_NAME).exe"

# macOS x86_64
# Requires: rustup target add x86_64-apple-darwin
#           On Linux: Install osxcross or use cross-rs
#           On macOS: Works natively
macos-x64:
	$(CARGO) build --release --target $(TARGET_MACOS_X64)
	@echo "Built: target/$(TARGET_MACOS_X64)/release/$(BINARY_NAME)"

# macOS ARM64 (Apple Silicon)
# Requires: rustup target add aarch64-apple-darwin
#           On Linux: Install osxcross or use cross-rs
#           On macOS: Works natively
macos-arm64:
	$(CARGO) build --release --target $(TARGET_MACOS_ARM64)
	@echo "Built: target/$(TARGET_MACOS_ARM64)/release/$(BINARY_NAME)"

# Build all targets (will fail if cross-compilers not installed)
all-targets: linux-x64 linux-arm64 windows macos-x64 macos-arm64

# Install Rust targets (still need system cross-compilers)
setup-targets:
	rustup target add $(TARGET_LINUX_X64)
	rustup target add $(TARGET_LINUX_ARM64)
	rustup target add $(TARGET_WINDOWS_X64)
	rustup target add $(TARGET_MACOS_X64)
	rustup target add $(TARGET_MACOS_ARM64)
	@echo ""
	@echo "Rust targets installed. You also need system cross-compilers:"
	@echo "  Linux ARM64: apt install gcc-aarch64-linux-gnu"
	@echo "  Windows:     apt install gcc-mingw-w64-x86-64"
	@echo "  macOS:       Use osxcross or build on macOS"

# Create distribution directory with all binaries
dist: linux-x64 linux-arm64 windows
	@mkdir -p $(DIST_DIR)
	cp target/$(TARGET_LINUX_X64)/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-linux-x64
	cp target/$(TARGET_LINUX_ARM64)/release/$(BINARY_NAME) $(DIST_DIR)/$(BINARY_NAME)-linux-arm64
	cp target/$(TARGET_WINDOWS_X64)/release/$(BINARY_NAME).exe $(DIST_DIR)/$(BINARY_NAME)-windows-x64.exe
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
	rm -rf /tmp/fat32expander-qemu

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

# Show filesystem info (usage: make info DEVICE=/path/to/device)
info: release
	./target/release/$(BINARY_NAME) info $(DEVICE)

# Dry run resize (usage: make dry-run DEVICE=/path/to/device)
dry-run: release
	./target/release/$(BINARY_NAME) resize --dry-run --verbose $(DEVICE)

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
	@echo "  make windows      Windows x86_64 (.exe)"
	@echo "  make macos-x64    macOS x86_64 (Intel)"
	@echo "  make macos-arm64  macOS ARM64 (Apple Silicon)"
	@echo "  make all-targets  Build all cross-compilation targets"
	@echo "  make dist         Build Linux + Windows and copy to dist/"
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
	@echo ""
	@echo "Utilities:"
	@echo "  make info DEVICE=<path>     Show filesystem info"
	@echo "  make dry-run DEVICE=<path>  Dry run resize"
	@echo "  make clean                  Clean build artifacts"
	@echo ""
	@echo "Cross-compilation prerequisites:"
	@echo "  Linux ARM64: apt install gcc-aarch64-linux-gnu"
	@echo "  Windows:     apt install gcc-mingw-w64-x86-64"
	@echo "  macOS:       Use osxcross or build on macOS natively"
	@echo ""
	@echo "Examples:"
	@echo "  make static && make test-qemu"
	@echo "  make test-qemu-one TEST=test-near-full"
	@echo "  make dist  # Build release binaries for distribution"
