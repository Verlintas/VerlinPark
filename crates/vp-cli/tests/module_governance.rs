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

#[test]
fn cli_entrypoint_modules_stay_bounded() {
    for (file, max) in [
        ("main.rs", 2_000),
        ("cli.rs", 5_000),
        ("dispatch.rs", 8_000),
        ("render.rs", 9_000),
        ("rpc.rs", 1_000),
        ("tests.rs", 9_000),
    ] {
        let lines = line_count(file);
        assert!(
            lines <= max,
            "src/{file} has {lines} lines; split below {max}"
        );
    }
}
