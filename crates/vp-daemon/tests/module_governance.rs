//! Structural governance for the `vp-daemon` entrypoint modules.

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

#[test]
fn daemon_entrypoint_modules_stay_bounded() {
    for (file, max) in [
        ("main.rs", 2_000),
        ("dispatch.rs", 2_000),
        ("http.rs", 1_000),
        ("sweepers.rs", 1_000),
        ("errors.rs", 1_000),
        ("server.rs", 1_000),
        ("backup_status.rs", 1_000),
        ("params.rs", 1_000),
        ("tests.rs", 2_000),
    ] {
        let lines = line_count(file);
        assert!(
            lines <= max,
            "src/{file} has {lines} lines; split below {max}"
        );
    }
    let root = manifest_dir().join("src").join("dispatch");
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            let lines = fs::read_to_string(&path).unwrap().lines().count();
            assert!(
                lines <= 1_500,
                "{} has {lines} lines; split below 1500",
                path.display()
            );
        }
    }
}
