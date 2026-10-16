use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

fn main() {
    // Set build timestamp using UNIX timestamp (simpler than formatting)
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=BUILD_TIMESTAMP={}", timestamp);

    // Get git commit hash if available
    let git_hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    // Rerun when HEAD moves. `.git/HEAD` alone only names the branch and does
    // not change on a commit; the commit lands in the branch ref, a loose file
    // or a line in packed-refs. A packed branch gets its loose file only with
    // the next commit, so the HEAD reflog is watched as well. Git resolves the
    // paths, so a worktree (where `.git` is a file) works too. Only existing
    // paths are listed: cargo treats a missing one as always changed and
    // would rebuild on every run.
    println!("cargo:rerun-if-changed=build.rs");
    let mut watched = vec![
        "HEAD".to_string(),
        "logs/HEAD".to_string(),
        "packed-refs".to_string(),
    ];
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"]) {
        watched.push(branch);
    }
    for name in watched {
        if let Some(path) = git(&["rev-parse", "--path-format=absolute", "--git-path", &name]) {
            if Path::new(&path).exists() {
                println!("cargo:rerun-if-changed={}", path);
            }
        }
    }
}

/// Run git and return its trimmed stdout, or `None` if git is missing or fails.
fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}
