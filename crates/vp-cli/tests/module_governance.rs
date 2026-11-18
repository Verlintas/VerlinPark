//! Structural governance for the `vp-cli` entrypoint modules.

use std::fs;
use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn line_count(file: &str) -> usize {
    let path = manifest_dir().join("src").join(file);
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        .lines()
        .count()
}

fn module_files(directory: &str) -> Vec<std::path::PathBuf> {
    let root = manifest_dir().join("src").join(directory);
    let mut files = Vec::new();
    let mut pending = vec![root];
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
fn cli_entrypoint_modules_stay_bounded() {
    for (file, max) in [
        ("main.rs", 2_000),
        ("cli.rs", 5_000),
        ("dispatch.rs", 2_000),
        ("render.rs", 2_000),
        ("rpc.rs", 1_000),
        ("tests.rs", 9_000),
    ] {
        let lines = line_count(file);
        assert!(
            lines <= max,
            "src/{file} has {lines} lines; split below {max}"
        );
    }
    for directory in ["dispatch", "render"] {
        for file in module_files(directory) {
            let lines = fs::read_to_string(&file).unwrap().lines().count();
            assert!(
                lines <= 1_500,
                "{} has {lines} lines; split below 1500",
                file.display()
            );
        }
    }
}
