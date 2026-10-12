//! Guard: integration tests keep their images under the cargo target
//! directory.
//!
//! Images run to gigabytes of sparse file. In the system temp directory they
//! land in /tmp whenever TMPDIR is unset, which is often a small root
//! filesystem or a tmpfs held in memory. Cargo creates `CARGO_TARGET_TMPDIR`
//! for integration tests, so every temp file and directory goes there.

#[test]
fn integration_tests_create_temp_files_under_the_target_dir() {
    let forbidden = ["NamedTempFile::new()", "tempdir()", "tempfile()"];
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") || path.ends_with("temp_files_test.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (number, line) in text.lines().enumerate() {
            if forbidden.iter().any(|f| line.contains(f)) {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use NamedTempFile::new_in(env!(\"CARGO_TARGET_TMPDIR\")) instead:\n{}",
        offenders.join("\n")
    );
}
