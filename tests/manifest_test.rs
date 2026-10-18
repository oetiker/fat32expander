//! Guard for [F-27]: a program that uses `fat32expander` as a library must not
//! compile the command-line dependencies.
//!
//! `clap` and `anyhow` are used only by the binary: `src/main.rs` and the
//! `src/cli/` modules it declares. As plain dependencies they were built for
//! every library consumer. They are now optional, switched on by the default
//! `cli` feature, which the binary requires; a consumer opts out with
//! `default-features = false`.
//!
//! Two things keep that true, and this file checks both: the manifest shape,
//! and a CI step that builds the library with no default features. The
//! manifest alone could declare the split while `src/lib.rs` quietly started
//! using `anyhow`; only that build would notice.

mod common;

use common::read;

/// The lines of one `[section]` of `Cargo.toml`, whitespace-normalised.
fn section(manifest: &str, name: &str) -> Vec<String> {
    let header = format!("[{name}]");
    manifest
        .lines()
        .map(str::trim)
        .skip_while(|line| *line != header)
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn cli_only_dependencies_are_optional() {
    let manifest = read("Cargo.toml");
    let deps = section(&manifest, "dependencies");
    for name in ["clap", "anyhow"] {
        let line = deps
            .iter()
            .find(|line| line.starts_with(&format!("{name} ")))
            .unwrap_or_else(|| panic!("`{name}` missing from [dependencies]: {deps:?}"));
        assert!(
            line.contains("optional = true"),
            "`{name}` must be optional so library consumers do not build it: {line}"
        );
    }
}

#[test]
fn the_default_cli_feature_enables_them_and_the_binary_requires_it() {
    let manifest = read("Cargo.toml");
    let features = section(&manifest, "features");
    assert!(
        features.iter().any(|l| l == r#"default = ["cli"]"#),
        "`cli` must be a default feature, so `cargo build` and `cargo install` \
         still produce the binary: {features:?}"
    );
    let cli = features
        .iter()
        .find(|l| l.starts_with("cli ="))
        .unwrap_or_else(|| panic!("no `cli` feature: {features:?}"));
    for dep in ["dep:clap", "dep:anyhow"] {
        assert!(cli.contains(dep), "`cli` must enable {dep}: {cli}");
    }

    let bin = section(&manifest, "[bin]");
    assert!(
        bin.iter().any(|l| l == r#"required-features = ["cli"]"#),
        "the binary must require `cli`, or a build without it fails to compile \
         main.rs instead of skipping it: {bin:?}"
    );
}

#[test]
fn ci_builds_the_library_without_default_features() {
    common::assert_ci_runs_ci_heavy();
    assert!(
        common::ci_heavy_recipe()
            .iter()
            .any(|line| line == "cargo check --lib --no-default-features"),
        "the `ci-heavy` recipe in the Makefile must build the library without \
         the `cli` feature; it is the only check that `src/lib.rs` does not \
         use `clap` or `anyhow`"
    );
}
