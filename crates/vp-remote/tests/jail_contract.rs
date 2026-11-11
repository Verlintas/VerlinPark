//! Path-jail contract tests through the public API.
//!
//! The helper's security depends on these properties: relative-only input,
//! canonical containment, final-symlink preservation for mutations, and no
//! access to Git metadata. These tests run against the same `PathJail` the
//! packaged helper uses.

use std::fs;
use std::os::unix::fs::symlink;

use tempfile::TempDir;
use vp_remote::PathJail;
use vp_remote::protocol::RemoteErrorCode;

fn jail() -> (TempDir, PathJail) {
    let dir = TempDir::new().unwrap();
    let jail = PathJail::new(dir.path()).unwrap();
    (dir, jail)
}

#[test]
fn control_characters_and_giant_paths_are_rejected() {
    for path in ["a\0b", "a\nb", "a\rb", "a\tb"] {
        let error = PathJail::validate_relative(path).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::InvalidParams, "{path:?}");
    }
    let long = "a".repeat(5000);
    assert_eq!(
        PathJail::validate_relative(&long).unwrap_err().code,
        RemoteErrorCode::InvalidParams
    );
}

#[test]
fn absolute_windows_style_and_parent_paths_are_rejected() {
    for path in ["/etc/passwd", "..", "a/..", "a/../../b", "//server/share"] {
        let error = PathJail::validate_relative(path).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PathEscape, "{path:?}");
    }
}

#[test]
fn unicode_and_spaces_are_preserved() {
    assert_eq!(
        PathJail::validate_relative("docs/план 1.md").unwrap(),
        "docs/план 1.md"
    );
    assert_eq!(
        PathJail::validate_relative("./space dir/file").unwrap(),
        "space dir/file"
    );
}

#[test]
fn git_metadata_is_rejected_case_insensitively() {
    let (_dir, jail) = jail();
    for path in [
        ".git/config",
        "sub/.GIT/config",
        ".gitmodules",
        "a/.Git/HEAD",
    ] {
        let error = jail.resolve_existing(path).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PermissionDenied, "{path}");
        let error = jail.resolve_write(path).unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PermissionDenied, "{path}");
    }
}

#[test]
fn chained_internal_symlinks_are_resolved() {
    let (dir, jail) = jail();
    fs::create_dir(dir.path().join("real")).unwrap();
    fs::write(dir.path().join("real/file"), "ok").unwrap();
    symlink("real", dir.path().join("link1")).unwrap();
    symlink("link1", dir.path().join("link2")).unwrap();
    let resolved = jail.resolve_existing("link2/file").unwrap();
    assert_eq!(
        resolved.absolute,
        dir.path().join("real/file").canonicalize().unwrap()
    );
}

#[test]
fn a_symlinked_parent_that_escapes_is_rejected() {
    let (dir, jail) = jail();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), dir.path().join("outside-link")).unwrap();
    for path in [
        "outside-link",
        "outside-link/new-file",
        "outside-link/sub/new-file",
    ] {
        let error = jail.resolve_write(path).unwrap_err();
        assert!(
            matches!(
                error.code,
                RemoteErrorCode::PathEscape | RemoteErrorCode::PermissionDenied
            ),
            "{path}: {error:?}"
        );
    }
}

#[test]
fn final_symlink_is_preserved_for_mutation() {
    let (dir, jail) = jail();
    fs::write(dir.path().join("target"), "ok").unwrap();
    symlink("target", dir.path().join("link")).unwrap();
    let resolved = jail.resolve_final("link").unwrap();
    assert_eq!(resolved.absolute, jail.root().join("link"));
    let mutation = jail.resolve_mutation("link").unwrap();
    assert_eq!(mutation.absolute, jail.root().join("link"));
}

#[test]
fn final_symlink_escape_is_rejected_for_metadata_and_mutation() {
    let (dir, jail) = jail();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("secret"), "no").unwrap();
    symlink(outside.path().join("secret"), dir.path().join("escape")).unwrap();
    assert_eq!(
        jail.resolve_final("escape").unwrap_err().code,
        RemoteErrorCode::PathEscape
    );
    assert_eq!(
        jail.resolve_mutation("escape").unwrap_err().code,
        RemoteErrorCode::PathEscape
    );
}

#[test]
fn worktree_root_is_rejected() {
    let (_dir, jail) = jail();
    assert!(jail.resolve_worktree(".").is_err());
    assert!(jail.resolve_worktree("worktrees/task").is_ok());
}

#[test]
fn missing_paths_report_path_not_found() {
    let (_dir, jail) = jail();
    assert_eq!(
        jail.resolve_existing("missing").unwrap_err().code,
        RemoteErrorCode::PathNotFound
    );
    assert_eq!(
        jail.resolve_final("missing").unwrap_err().code,
        RemoteErrorCode::PathNotFound
    );
    assert_eq!(
        jail.resolve_mutation("missing").unwrap_err().code,
        RemoteErrorCode::PathNotFound
    );
}

#[test]
fn relative_rendering_is_forward_slash_and_dot_for_root() {
    let (dir, jail) = jail();
    assert_eq!(jail.relative_path(jail.root()), Some(".".to_string()));
    fs::create_dir(dir.path().join("sub")).unwrap();
    assert_eq!(
        jail.relative_path(&jail.root().join("sub")),
        Some("sub".to_string())
    );
}

#[test]
fn root_metadata_is_available_but_root_cannot_be_mutated() {
    let (_dir, jail) = jail();
    assert_eq!(jail.resolve_final(".").unwrap().relative, ".");
    assert_eq!(
        jail.resolve_mutation(".").unwrap_err().code,
        RemoteErrorCode::PermissionDenied
    );
}

#[test]
fn write_target_parent_is_the_nearest_existing_directory() {
    let (dir, jail) = jail();
    fs::create_dir_all(dir.path().join("a/b")).unwrap();
    let resolved = jail.resolve_write("a/b/c/d.txt").unwrap();
    assert!(!resolved.exists);
    assert_eq!(
        resolved.parent,
        dir.path().join("a/b").canonicalize().unwrap()
    );
}

#[test]
fn an_existing_directory_is_not_accepted_as_a_write_target_by_metadata() {
    let (dir, jail) = jail();
    fs::create_dir(dir.path().join("d")).unwrap();
    let resolved = jail.resolve_write("d").unwrap();
    assert!(resolved.exists);
    assert_eq!(resolved.target, jail.root().join("d"));
}
