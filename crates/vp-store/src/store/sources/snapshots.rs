use super::*;

impl Store {
    /// Persist the durable source snapshot before/after the SQLite projection.
    /// A failed append must not be treated as a successful source add.
    pub fn write_source_snapshot(&self, snapshot: &SourceSnapshot) -> Result<()> {
        let dir = self.source_dir(&snapshot.source.project_id);
        paths::ensure_private_dir(&dir)?;
        let path = dir.join(format!("{}.json", snapshot.source.id));
        let temp = dir.join(format!(".tmp-{}.json", Uuid::now_v7()));
        let bytes = serde_json::to_vec(snapshot)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .with_context(|| format!("creating source snapshot {}", temp.display()))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, &path)
            .with_context(|| format!("replacing source snapshot {}", path.display()))?;
        paths::ensure_private_file(&path)?;
        Ok(())
    }

    pub fn read_source_snapshot(
        &self,
        project_id: &str,
        source_id: &str,
    ) -> Result<Option<SourceSnapshot>> {
        let path = self
            .source_dir(project_id)
            .join(format!("{}.json", source_id));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).with_context(|| {
                format!("parsing source snapshot {}", path.display())
            })?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn delete_source_snapshot(&self, project_id: &str, source_id: &str) -> Result<()> {
        let path = self
            .source_dir(project_id)
            .join(format!("{}.json", source_id));
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("removing {}", path.display())),
        }
    }

    /// All durable snapshots, used by `db-rebuild` and source recovery.
    pub fn list_source_snapshots(&self) -> Result<Vec<SourceSnapshot>> {
        let root = self.data_dir.join("sources");
        let mut snapshots = Vec::new();
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", root.display())),
        };
        for project in entries {
            let project = match project {
                Ok(project) => project,
                Err(_) => continue,
            };
            if !project
                .file_type()
                .map(|kind| kind.is_dir())
                .unwrap_or(false)
            {
                continue;
            }
            let files = match std::fs::read_dir(project.path()) {
                Ok(files) => files,
                Err(_) => continue,
            };
            for file in files {
                let file = match file {
                    Ok(file) => file,
                    Err(_) => continue,
                };
                if file.path().extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let bytes = match std::fs::read(file.path()) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        tracing::warn!(%error, path = %file.path().display(), "could not read source snapshot");
                        continue;
                    }
                };
                match serde_json::from_slice::<SourceSnapshot>(&bytes) {
                    Ok(snapshot) => snapshots.push(snapshot),
                    Err(error) => {
                        tracing::warn!(%error, path = %file.path().display(), "skipping malformed source snapshot");
                    }
                }
            }
        }
        Ok(snapshots)
    }
}
