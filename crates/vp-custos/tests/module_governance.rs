use std::fs;
use std::path::{Path, PathBuf};

const MAX_PARENT_LINES: usize = 2_000;
const MAX_MODULE_LINES: usize = 3_000;

#[test]
fn custos_modules_stay_bounded() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let lib = root.join("lib.rs");
    let lines = line_count(&lib);
    assert!(
        lines <= MAX_PARENT_LINES,
        "{} has {lines} lines (max {MAX_PARENT_LINES})",
        lib.display()
    );
    for path in rust_files(&root) {
        if path == lib {
            continue;
        }
        let lines = line_count(&path);
        assert!(
            lines <= MAX_MODULE_LINES,
            "{} has {lines} lines (max {MAX_MODULE_LINES})",
            path.display()
        );
    }
}

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

fn line_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|text| text.lines().count())
        .unwrap_or(0)
}
