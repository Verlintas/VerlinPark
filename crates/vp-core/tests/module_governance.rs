//! Structural governance for the `vp-core` source trees.
//!
//! M3.2/M3.3 split the original 20k+ line catch-all files into parent modules
//! and nested domain modules. These checks read the source tree at test time:
//! they are a simple regression guard against recreating a giant module, not a
//! statement about runtime behavior.

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
        let entries = fs::read_dir(&current)
            .unwrap_or_else(|error| panic!("read {}: {error}", current.display()));
        for entry in entries {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            if path.file_name().is_some_and(|name| name == "tests.rs") {
                continue;
            }
            files.push(path);
        }
    }
    files.sort();
    files
}

#[test]
fn core_parent_files_stay_bounded() {
    let root = manifest_dir().join("src");
    for parent in ["lib.rs", "agent.rs"] {
        let lines = line_count(&root.join(parent));
        assert!(
            lines <= MAX_PARENT_LINES,
            "src/{parent} has {lines} lines; keep the parent under {MAX_PARENT_LINES}"
        );
    }
}

#[test]
fn core_domain_modules_stay_bounded() {
    let root = manifest_dir().join("src");
    for directory in ["agent", "engine"] {
        let path = root.join(directory);
        assert!(
            path.is_dir(),
            "missing domain module directory: {}",
            path.display()
        );
        for file in module_files(&path) {
            let lines = line_count(&file);
            assert!(
                lines <= MAX_MODULE_LINES,
                "{} has {lines} lines; split it below {MAX_MODULE_LINES}",
                file.display()
            );
        }
    }
}

#[test]
fn core_module_split_keeps_the_expected_entry_points() {
    let root = manifest_dir().join("src");
    assert!(root.join("agent").join("tests.rs").is_file());
    assert!(root.join("tests.rs").is_file());
    assert!(root.join("engine").is_dir());
    let lib = fs::read_to_string(root.join("lib.rs")).unwrap();
    assert!(lib.contains("pub mod agent;"));
    assert!(lib.contains("mod engine;"));
    assert!(lib.contains("#[path = \"tests.rs\"]"));
}
