# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### New

### Changed

### Fixed

## 0.0.2 - 2025-12-02
### New
- added a note to the readme why we are not shrinking
- added `ClusterId` and `SectorNum` newtype wrappers for improved type safety
- added `Debug` trait implementation for `Device`

### Changed
- replaced `String` with `PathBuf` for device paths throughout the codebase
- public API functions now accept `impl AsRef<Path>` instead of `&str`
- `ResizeOptions` now uses builder pattern: `ResizeOptions::new(path).dry_run(true)`
- replaced glob re-exports (`pub use module::*`) with explicit exports in `fat32/mod.rs` and `resize/mod.rs`
- extracted duplicate logic in `Device::open()` and `open_readonly()` into shared `open_impl()` helper
- extracted `print_verbose_resize_info()` helper from `resize_fat32()`
- used `let-else` pattern for cleaner checkpoint parsing

## 0.0.1 - 2025-12-01
### New
- initial release


