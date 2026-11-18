//! Structural governance for the `vp-store` domain modules.

use std::fs;
use std::path::{Path, PathBuf};

const MAX_PARENT_LINES: usize = 8_000;
const MAX_MODULE_LINES: usize = 3_000;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn line_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .lines()
        .count()
}

fn module_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(&current)
            .unwrap_or_else(|error| panic!("read {}: {error}", current.display()))
        {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && path.file_name().is_some_and(|name| name != "tests.rs")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

#[test]
fn store_parent_and_modules_stay_bounded() {
    let root = manifest_dir().join("src");
    let parent = line_count(&root.join("store.rs"));
    assert!(
        parent <= MAX_PARENT_LINES,
        "src/store.rs has {parent} lines; split below {MAX_PARENT_LINES}"
    );
    let modules = module_files(&root.join("store"));
    assert!(
        modules.len() >= 40,
        "store module tree unexpectedly disappeared"
    );
    for file in modules {
        let lines = line_count(&file);
        assert!(
            lines <= MAX_MODULE_LINES,
            "{} has {lines} lines; split below {MAX_MODULE_LINES}",
            file.display()
        );
    }
}
