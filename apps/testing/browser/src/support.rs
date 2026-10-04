use anyhow::Result;
pub use snap_browser_tests::support::*;
use std::path::{Path, PathBuf};

pub struct SourceCopy {
    pub root: PathBuf,
    directory: tempfile::TempDir,
}
impl SourceCopy {
    pub fn new() -> Result<Self> {
        let directory = scratch("snap-browser-source-")?;
        let destination = directory.path().to_owned();
        let source = root();
        for path in [
            "Cargo.toml",
            "Cargo.lock",
            "package.json",
            "bun.lock",
            "tsconfig.json",
            "crates",
            "kits",
            "tools/cli",
            "apps",
            "tests",
        ] {
            copy_tree(&source.join(path), &destination.join(path))?;
        }
        for path in ["node_modules", ".tools", "target"] {
            std::os::unix::fs::symlink(source.join(path), destination.join(path))?;
        }
        Ok(Self {
            root: destination,
            directory,
        })
    }
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.is_dir() {
        std::fs::create_dir_all(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            // Reuse Bun's workspace-local dependency links without copying caches.
            if entry.file_name() == "node_modules" {
                std::os::unix::fs::symlink(entry.path(), destination.join(entry.file_name()))?;
                continue;
            }
            if matches!(
                entry.file_name().to_str(),
                Some(".snap" | ".deployment" | "target" | "build" | "dist" | ".git" | ".tmp")
            ) {
                continue;
            }
            copy_tree(&entry.path(), &destination.join(entry.file_name()))?;
        }
    } else if metadata.file_type().is_symlink() {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(std::fs::read_link(source)?, destination)?;
    } else {
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, destination)?;
    }
    Ok(())
}
