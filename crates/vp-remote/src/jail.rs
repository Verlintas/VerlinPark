//! Canonical path jail for a remote workspace root.
//!
//! Every helper operation resolves a caller-supplied *relative* path through
//! this jail. The jail rejects absolute paths, parent traversal, and symlinks
//! that resolve outside the canonical root. Write targets additionally reject
//! existing symlinks and Git metadata so an atomic rename cannot be used to
//! replace repository internals.
//!
//! This is a path-containment check, not an operating-system sandbox. The
//! helper runs with the remote user's authority; a malicious local process on
//! that host can still race the jail. The daemon treats remote hosts as
//! trusted infrastructure (see ADR-0156).

use std::path::{Component, Path, PathBuf};

use crate::protocol::{RemoteError, RemoteErrorCode};

const MAX_RELATIVE_PATH_CHARS: usize = 4096;

#[derive(Debug, Clone)]
pub struct PathJail {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWrite {
    /// Absolute target under the canonical root.
    pub target: PathBuf,
    /// Existing parent directory. The server creates missing descendants only
    /// after this parent has been checked.
    pub parent: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    pub absolute: PathBuf,
    pub relative: String,
}

impl PathJail {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, RemoteError> {
        let requested = root.as_ref();
        let canonical = requested.canonicalize().map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::PathNotFound,
                format!(
                    "remote root does not exist: {} ({error})",
                    requested.display()
                ),
            )
        })?;
        if !canonical.is_dir() {
            return Err(RemoteError::new(
                RemoteErrorCode::NotADirectory,
                format!("remote root is not a directory: {}", canonical.display()),
            ));
        }
        Ok(Self { root: canonical })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Normalize and validate a caller-provided relative path. This does not
    /// touch the filesystem and is safe to use before any I/O.
    pub fn validate_relative(path: &str) -> Result<String, RemoteError> {
        if path.is_empty() {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "path must not be empty; use \".\" for the workspace root",
            ));
        }
        if path.chars().count() > MAX_RELATIVE_PATH_CHARS {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                format!("path exceeds {MAX_RELATIVE_PATH_CHARS} characters"),
            ));
        }
        if path.chars().any(char::is_control) {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "path must not contain control characters",
            ));
        }
        let parsed = Path::new(path);
        if parsed.is_absolute() {
            return Err(RemoteError::new(
                RemoteErrorCode::PathEscape,
                format!("absolute paths are not allowed: {path}"),
            )
            .with_hint("use a path relative to the configured remote root"));
        }
        let mut normalized = Vec::new();
        for component in parsed.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(value) => {
                    let value = value.to_string_lossy();
                    if value == ".." {
                        return Err(RemoteError::new(
                            RemoteErrorCode::PathEscape,
                            format!("parent traversal is not allowed: {path}"),
                        ));
                    }
                    normalized.push(value.to_string());
                }
                Component::ParentDir => {
                    return Err(RemoteError::new(
                        RemoteErrorCode::PathEscape,
                        format!("parent traversal is not allowed: {path}"),
                    ));
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(RemoteError::new(
                        RemoteErrorCode::PathEscape,
                        format!("absolute paths are not allowed: {path}"),
                    ));
                }
            }
        }
        if normalized.is_empty() {
            Ok(".".to_string())
        } else {
            Ok(normalized.join("/"))
        }
    }

    fn reject_git_metadata(relative: &str) -> Result<(), RemoteError> {
        if relative.split('/').any(|component| {
            component.eq_ignore_ascii_case(".git") || component.eq_ignore_ascii_case(".gitmodules")
        }) {
            return Err(RemoteError::new(
                RemoteErrorCode::PermissionDenied,
                "Git metadata is not accessible through the file operations",
            )
            .with_hint("use the structured git operations instead"));
        }
        Ok(())
    }

    fn ensure_inside(&self, candidate: &Path) -> Result<(), RemoteError> {
        if !candidate.starts_with(&self.root) {
            return Err(RemoteError::new(
                RemoteErrorCode::PathEscape,
                format!(
                    "resolved path escapes the remote root: {}",
                    candidate.display()
                ),
            )
            .with_hint("symlinks must resolve inside the configured remote root"));
        }
        Ok(())
    }

    fn relative_to_root(&self, absolute: &Path) -> String {
        absolute
            .strip_prefix(&self.root)
            .map(|path| {
                let rendered = path.to_string_lossy().replace('\\', "/");
                if rendered.is_empty() {
                    ".".to_string()
                } else {
                    rendered
                }
            })
            .unwrap_or_else(|_| ".".to_string())
    }

    pub fn root_path(&self) -> ResolvedPath {
        ResolvedPath {
            absolute: self.root.clone(),
            relative: ".".to_string(),
        }
    }

    /// Resolve an existing path for reading or inspection.
    pub fn resolve_existing(&self, path: &str) -> Result<ResolvedPath, RemoteError> {
        let relative = Self::validate_relative(path)?;
        Self::reject_git_metadata(&relative)?;
        let candidate = if relative == "." {
            self.root.clone()
        } else {
            self.root.join(&relative)
        };
        let canonical = candidate.canonicalize().map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::NotFound {
                RemoteErrorCode::PathNotFound
            } else if error.kind() == std::io::ErrorKind::PermissionDenied {
                RemoteErrorCode::PermissionDenied
            } else {
                RemoteErrorCode::Internal
            };
            RemoteError::new(
                code,
                format!("cannot resolve {relative} under the remote root: {error}"),
            )
        })?;
        self.ensure_inside(&canonical)?;
        Self::reject_git_metadata(&self.relative_to_root(&canonical))?;
        Ok(ResolvedPath {
            absolute: canonical,
            relative,
        })
    }

    /// Resolve a path for writing. Missing components are allowed; every
    /// existing prefix, including the final target, must stay inside the root.
    pub fn resolve_write(&self, path: &str) -> Result<ResolvedWrite, RemoteError> {
        let relative = Self::validate_relative(path)?;
        if relative == "." {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "cannot write to the remote root itself",
            ));
        }
        Self::reject_git_metadata(&relative)?;
        let mut current = self.root.clone();
        let components = relative.split('/').collect::<Vec<_>>();
        let mut missing_from: Option<usize> = None;
        for (index, component) in components.iter().enumerate() {
            current.push(component);
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        let resolved = current.canonicalize().map_err(|error| {
                            RemoteError::new(
                                RemoteErrorCode::PathEscape,
                                format!("symlink target cannot be resolved: {error}"),
                            )
                        })?;
                        self.ensure_inside(&resolved)?;
                        // Writes must not replace or traverse a symlink. Reads
                        // may follow an inside-root symlink, but a write that
                        // atomically renames over a link is surprising and can
                        // be used to pin a path to a different file later.
                        return Err(RemoteError::new(
                            RemoteErrorCode::PermissionDenied,
                            format!("refusing to write through symlink: {relative}"),
                        )
                        .with_hint("write the real file or remove the link first"));
                    }
                    if index + 1 == components.len() {
                        self.ensure_inside(&current)?;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing_from = Some(index);
                    break;
                }
                Err(error) => {
                    return Err(RemoteError::new(
                        if error.kind() == std::io::ErrorKind::PermissionDenied {
                            RemoteErrorCode::PermissionDenied
                        } else {
                            RemoteErrorCode::Internal
                        },
                        format!("cannot inspect {relative}: {error}"),
                    ));
                }
            }
        }
        let exists = missing_from.is_none();
        let parent_index = if exists {
            components.len().saturating_sub(1)
        } else {
            missing_from.unwrap_or(0)
        };
        let parent_relative = components[..parent_index].join("/");
        let parent = if parent_relative.is_empty() {
            self.root.clone()
        } else {
            let parent_candidate = self.root.join(&parent_relative);
            let canonical = parent_candidate.canonicalize().map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::PathNotFound,
                    format!("parent directory for {relative} does not exist: {error}"),
                )
                .with_hint("set create_parents=true to create missing directories")
            })?;
            self.ensure_inside(&canonical)?;
            canonical
        };
        let target = self.root.join(&relative);
        if let Some(existing_parent) = target.parent()
            && !existing_parent.starts_with(&self.root)
        {
            return Err(RemoteError::new(
                RemoteErrorCode::PathEscape,
                format!("write target escapes the remote root: {relative}"),
            ));
        }
        Ok(ResolvedWrite {
            target,
            parent,
            exists,
        })
    }

    /// Resolve an existing path that is about to be removed or renamed.
    ///
    /// Unlike `resolve_existing`, this preserves a final symlink instead of
    /// following it so removing a link removes the link, not its target. The
    /// link target must still resolve inside the root.
    pub fn resolve_mutation(&self, path: &str) -> Result<ResolvedPath, RemoteError> {
        let relative = Self::validate_relative(path)?;
        if relative == "." {
            return Err(RemoteError::new(
                RemoteErrorCode::PermissionDenied,
                "the remote root cannot be removed or renamed",
            ));
        }
        Self::reject_git_metadata(&relative)?;
        let candidate = self.root.join(&relative);
        let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
            RemoteError::new(
                if error.kind() == std::io::ErrorKind::NotFound {
                    RemoteErrorCode::PathNotFound
                } else {
                    RemoteErrorCode::Internal
                },
                format!("cannot inspect {relative}: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            let target = candidate.canonicalize().map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::PathEscape,
                    format!("symlink target cannot be resolved: {error}"),
                )
            })?;
            self.ensure_inside(&target)?;
        } else {
            let canonical = candidate.canonicalize().map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::Internal,
                    format!("cannot resolve {relative}: {error}"),
                )
            })?;
            self.ensure_inside(&canonical)?;
        }
        if candidate == self.root {
            return Err(RemoteError::new(
                RemoteErrorCode::PermissionDenied,
                "the remote root cannot be removed or renamed",
            ));
        }
        Ok(ResolvedPath {
            absolute: candidate,
            relative,
        })
    }

    /// Resolve an existing path for metadata without following a final
    /// symlink. The link target must still resolve inside the root.
    pub fn resolve_final(&self, path: &str) -> Result<ResolvedPath, RemoteError> {
        let relative = Self::validate_relative(path)?;
        Self::reject_git_metadata(&relative)?;
        let candidate = if relative == "." {
            self.root.clone()
        } else {
            self.root.join(&relative)
        };
        let metadata = std::fs::symlink_metadata(&candidate).map_err(|error| {
            RemoteError::new(
                if error.kind() == std::io::ErrorKind::NotFound {
                    RemoteErrorCode::PathNotFound
                } else if error.kind() == std::io::ErrorKind::PermissionDenied {
                    RemoteErrorCode::PermissionDenied
                } else {
                    RemoteErrorCode::Internal
                },
                format!("cannot inspect {relative}: {error}"),
            )
        })?;
        if metadata.file_type().is_symlink() {
            let target = candidate.canonicalize().map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::PathEscape,
                    format!("symlink target cannot be resolved: {error}"),
                )
            })?;
            self.ensure_inside(&target)?;
            let target_relative = self.relative_to_root(&target);
            Self::reject_git_metadata(&target_relative)?;
        }
        Ok(ResolvedPath {
            absolute: candidate,
            relative,
        })
    }

    /// Render an absolute path that is known to be inside the root as a
    /// root-relative path with forward slashes.
    pub fn relative_path(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let rendered = relative.to_string_lossy().replace('\\', "/");
        if rendered.is_empty() {
            Some(".".to_string())
        } else {
            Some(rendered)
        }
    }

    /// Resolve a worktree path. The target may not exist yet, but it must be a
    /// root-relative path and must not be the root itself.
    pub fn resolve_worktree(&self, path: &str) -> Result<PathBuf, RemoteError> {
        let resolved = self.resolve_write(path)?;
        if resolved.target == self.root {
            return Err(RemoteError::new(
                RemoteErrorCode::InvalidParams,
                "worktree path must not be the remote root",
            ));
        }
        Ok(resolved.target)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn jail() -> (TempDir, PathJail) {
        let dir = TempDir::new().unwrap();
        let jail = PathJail::new(dir.path()).unwrap();
        (dir, jail)
    }

    #[test]
    fn rejects_absolute_and_parent_paths() {
        assert_eq!(
            PathJail::validate_relative("/etc/passwd").unwrap_err().code,
            RemoteErrorCode::PathEscape
        );
        assert_eq!(
            PathJail::validate_relative("../outside").unwrap_err().code,
            RemoteErrorCode::PathEscape
        );
        assert_eq!(
            PathJail::validate_relative("a/../../b").unwrap_err().code,
            RemoteErrorCode::PathEscape
        );
        assert_eq!(PathJail::validate_relative("./a//b").unwrap(), "a/b");
        assert_eq!(PathJail::validate_relative(".").unwrap(), ".");
    }

    #[test]
    fn normalizes_cur_dir_components() {
        assert_eq!(PathJail::validate_relative("./a/./b").unwrap(), "a/b");
        assert_eq!(PathJail::validate_relative("a/b/").unwrap(), "a/b");
    }

    #[test]
    fn existing_symlink_inside_root_is_accepted_for_reads() {
        let (dir, jail) = jail();
        fs::write(dir.path().join("file.txt"), "hello").unwrap();
        std::os::unix::fs::symlink("file.txt", dir.path().join("link.txt")).unwrap();
        let resolved = jail.resolve_existing("link.txt").unwrap();
        assert_eq!(
            resolved.absolute,
            dir.path().join("file.txt").canonicalize().unwrap()
        );
    }

    #[test]
    fn symlink_escape_is_rejected_for_reads() {
        let (dir, jail) = jail();
        let outside = TempDir::new().unwrap();
        fs::write(outside.path().join("secret"), "no").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let error = jail.resolve_existing("escape/secret").unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PathEscape);
    }

    #[test]
    fn symlink_escape_is_rejected_for_writes() {
        let (dir, jail) = jail();
        let outside = TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let error = jail.resolve_write("escape/file").unwrap_err();
        assert!(matches!(
            error.code,
            RemoteErrorCode::PermissionDenied | RemoteErrorCode::PathEscape
        ));
    }

    #[test]
    fn symlink_inside_root_is_rejected_for_writes() {
        let (dir, jail) = jail();
        fs::write(dir.path().join("real"), "x").unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
        let error = jail.resolve_write("link").unwrap_err();
        assert_eq!(error.code, RemoteErrorCode::PermissionDenied);
    }

    #[test]
    fn git_metadata_is_never_exposed() {
        let (dir, jail) = jail();
        fs::create_dir(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "secret").unwrap();
        assert_eq!(
            jail.resolve_existing(".git/config").unwrap_err().code,
            RemoteErrorCode::PermissionDenied
        );
        assert_eq!(
            jail.resolve_write(".git/config").unwrap_err().code,
            RemoteErrorCode::PermissionDenied
        );
    }

    #[test]
    fn missing_write_target_is_allowed_only_under_an_existing_parent() {
        let (dir, jail) = jail();
        fs::create_dir(dir.path().join("sub")).unwrap();
        let resolved = jail.resolve_write("sub/new.txt").unwrap();
        assert!(!resolved.exists);
        assert_eq!(
            resolved.parent,
            dir.path().join("sub").canonicalize().unwrap()
        );
        let missing_parent = jail.resolve_write("absent/new.txt").unwrap();
        assert!(!missing_parent.exists);
        assert_eq!(missing_parent.parent, jail.root().to_path_buf());
    }

    #[test]
    fn root_cannot_be_removed() {
        let (_dir, jail) = jail();
        assert_eq!(
            jail.resolve_mutation(".").unwrap_err().code,
            RemoteErrorCode::PermissionDenied
        );
    }
}
