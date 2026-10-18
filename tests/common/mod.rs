//! Helpers shared by the tests that guard the CI setup.
//!
//! CI runs the heavy suite as `make ci-heavy` inside the Containerfile image:
//! `.github/workflows/ci-local.yml` calls `scripts/ci-container.sh ci-heavy`,
//! which runs that make target. The guards therefore read the recipe, and
//! check that the workflow still runs it.

use std::path::PathBuf;

pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn read(relative: &str) -> String {
    let path = repo_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// The command lines of the `ci-heavy` recipe in the Makefile, with
/// `$(CARGO)` spelled `cargo` and whitespace normalised.
pub fn ci_heavy_recipe() -> Vec<String> {
    let makefile = read("Makefile");
    let recipe: Vec<String> = makefile
        .lines()
        .skip_while(|line| !line.starts_with("ci-heavy:"))
        .skip(1)
        .take_while(|line| line.starts_with('\t'))
        .map(|line| {
            line.replace("$(CARGO)", "cargo")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert!(
        !recipe.is_empty(),
        "no `ci-heavy` recipe in the Makefile -- CI no longer runs the heavy \
         suite, or the target was renamed and these guards went blind"
    );
    recipe
}

/// Fails unless the CI workflow runs `ci-heavy` in the container.
pub fn assert_ci_runs_ci_heavy() {
    let workflow = read(".github/workflows/ci-local.yml");
    assert!(
        workflow.contains("scripts/ci-container.sh ci-heavy"),
        "ci-local.yml no longer runs `scripts/ci-container.sh ci-heavy`, so the \
         Makefile recipe these guards read is not what CI runs"
    );
}
