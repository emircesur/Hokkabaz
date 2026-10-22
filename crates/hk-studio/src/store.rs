use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::{Project, Result, StudioError, invalid};

#[derive(Debug)]
pub(crate) struct ProjectStore {
    root: PathBuf,
    lock: Mutex<()>,
}

impl ProjectStore {
    pub fn new(root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            lock: Mutex::new(()),
        })
    }

    pub fn dir(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || id.len() > 80
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(invalid("invalid project id"));
        }
        Ok(self.root.join(id))
    }

    pub fn load(&self, id: &str) -> Result<Project> {
        Ok(serde_json::from_slice(&std::fs::read(
            self.dir(id)?.join("project.json"),
        )?)?)
    }

    pub fn save(&self, project: &mut Project, expected: Option<u64>) -> Result<()> {
        let _guard = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = self.dir(&project.id)?;
        if let Some(revision) = expected {
            if self.load(&project.id)?.revision != revision {
                return Err(StudioError::Conflict);
            }
        } else if dir.join("project.json").exists() {
            return Err(StudioError::Conflict);
        }
        std::fs::create_dir_all(&dir)?;
        project.revision += 1;
        atomic_json(&dir.join("project.json"), project)
    }

    pub fn list(&self) -> Result<Vec<Project>> {
        let mut projects: Vec<Project> = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path().join("project.json");
            if path.exists() {
                projects.push(serde_json::from_slice(&std::fs::read(path)?)?);
            }
        }
        projects.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(projects)
    }
}

pub(crate) fn atomic_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec_pretty(value)?)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    result
}
