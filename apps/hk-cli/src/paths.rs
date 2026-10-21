//! On-disk layout: `<data>/sessions`, `<data>/hk.db`, `<data>/spill`.

use std::path::PathBuf;

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
}

impl Paths {
    /// `--data-dir` / `HOKKABAZ_HOME`, else `~/.hokkabaz`.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let data = match explicit {
            Some(p) => p,
            None => {
                let home = std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .context("cannot locate home dir; pass --data-dir or set HOKKABAZ_HOME")?;
                PathBuf::from(home).join(".hokkabaz")
            }
        };
        Ok(Self { data })
    }

    pub fn sessions(&self) -> PathBuf {
        self.data.join("sessions")
    }

    pub fn db(&self) -> PathBuf {
        self.data.join("hk.db")
    }

    pub fn spill(&self) -> PathBuf {
        self.data.join("spill")
    }
}
