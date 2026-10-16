//! Guard for [F-R61]: every integration test binary must actually be executed
//! by CI.
//!
//! The flaw this prevents is not a bug in the code but a gap in the harness.
//! `tests.yml` used to name its targets one by one -- `--test integration_test`,
//! `--test durability_test` -- so three test binaries totalling 30 tests, two of
//! them written as safety nets by earlier fix sessions, ran only on developers'
//! machines. A safety net CI does not run cannot fail a pull request, which is
//! the only thing that makes it a net.
//!
//! Naming targets individually fails silently and in the safe-looking
//! direction: the workflow stays green, the summary still says "test result:
//! ok", and the omission is visible only to someone who diffs `ls tests/`
//! against the YAML. This test does that diff on every run.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

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

/// The `cargo test` command lines in the workflow, one per line, whitespace
/// normalised so a line continuation does not hide a flag.
fn cargo_test_invocations(workflow: &str) -> Vec<String> {
    let joined = workflow.replace("\\\n", " ");
    joined
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| line.contains("cargo test"))
        .collect()
}

#[test]
fn ci_executes_every_integration_test_binary() {
    let root = repo_root();
    let workflow_path = root.join(".github/workflows/tests.yml");
    let workflow = std::fs::read_to_string(&workflow_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", workflow_path.display()));

    let invocations = cargo_test_invocations(&workflow);
    assert!(
        !invocations.is_empty(),
        "no `cargo test` invocation found in {} -- the workflow no longer runs \
         tests at all, or its shape changed enough that this guard is blind",
        workflow_path.display()
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
    let root = repo_root();
    let workflow = std::fs::read_to_string(root.join(".github/workflows/tests.yml"))
        .expect("cannot read .github/workflows/tests.yml");

    assert!(
        cargo_test_invocations(&workflow).iter().any(|line| {
            line.contains("--features device-journal") && line.contains("--test durability_test")
        }),
        "no `cargo test --features device-journal --test durability_test` in CI. \
         A plain --all-targets run compiles that suite to nothing and still \
         reports ok, so dropping this invocation silently deletes the \
         durability tests."
    );
}
