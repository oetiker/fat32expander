//! Guard for [F-R61]: every integration test binary must actually be executed
//! by CI.
//!
//! The flaw this prevents is not a bug in the code but a gap in the harness.
//! The CI workflow used to name its targets one by one -- `--test
//! integration_test`, `--test durability_test` -- so three test binaries
//! totalling 30 tests, two of them written as safety nets by earlier fix
//! sessions, ran only on developers' machines. A safety net CI does not run
//! cannot fail a pull request, which is the only thing that makes it a net.
//!
//! Naming targets individually fails silently and in the safe-looking
//! direction: CI stays green, the summary still says "test result: ok", and
//! the omission is visible only to someone who diffs `ls tests/` against the
//! CI commands. This test does that diff on every run, against the
//! `ci-heavy` recipe in the Makefile, which CI runs in the container.
//! The plain `cargo test` of the repo-infra piece `ri-ci-rust` covers the
//! tests that are not ignored.

mod common;

use std::collections::BTreeSet;
use std::path::Path;

/// Every `tests/*.rs` file, by target name (the file stem).
fn test_target_names(root: &Path) -> BTreeSet<String> {
    std::fs::read_dir(root.join("tests"))
        .expect("tests/ directory must exist")
        .filter_map(|entry| {
            let path = entry.expect("failed to read a tests/ entry").path();
            if path.extension()? != "rs" {
                return None;
            }
            Some(path.file_stem()?.to_string_lossy().into_owned())
        })
        .collect()
}

/// The `cargo test` command lines CI runs in the container.
fn cargo_test_invocations() -> Vec<String> {
    common::assert_ci_runs_ci_heavy();
    common::ci_heavy_recipe()
        .into_iter()
        .filter(|line| line.contains("cargo test"))
        .collect()
}

#[test]
fn ci_executes_every_integration_test_binary() {
    let root = common::repo_root();
    let invocations = cargo_test_invocations();
    assert!(
        !invocations.is_empty(),
        "no `cargo test` invocation in the `ci-heavy` recipe -- CI no longer \
         runs the ignored tests at all, or its shape changed enough that this \
         guard is blind"
    );

    // A target is covered either by a sweep (`--all-targets` / `--tests`, which
    // pick up new files automatically) or by being named outright.
    let swept = invocations
        .iter()
        .any(|line| line.contains("--all-targets") || line.contains("--tests"));

    let missing: Vec<String> = test_target_names(&root)
        .into_iter()
        .filter(|target| {
            if swept {
                return false;
            }
            let named = format!("--test {target}");
            !invocations.iter().any(|line| line.contains(&named))
        })
        .collect();

    assert!(
        missing.is_empty(),
        "these test binaries exist in tests/ but CI never runs them: {}\n\
         Add a sweeping invocation (`cargo test --all-targets`) rather than \
         naming the missing ones -- naming targets individually is what \
         produced [F-R61] in the first place.\n\
         cargo test invocations found:\n  {}",
        missing.join(", "),
        invocations.join("\n  ")
    );
}

/// The durability suite is the one target a sweep cannot cover: without
/// `--features device-journal` it compiles to zero tests, so `--all-targets`
/// runs it and reports success while asserting nothing. It must keep its own
/// explicitly-featured invocation.
#[test]
fn the_durability_suite_keeps_its_explicit_feature_invocation() {
    assert!(
        cargo_test_invocations().iter().any(|line| {
            line.contains("--features device-journal") && line.contains("--test durability_test")
        }),
        "no `cargo test --features device-journal --test durability_test` in the \
         `ci-heavy` recipe. \
         A plain --all-targets run compiles that suite to nothing and still \
         reports ok, so dropping this invocation silently deletes the \
         durability tests."
    );
}
