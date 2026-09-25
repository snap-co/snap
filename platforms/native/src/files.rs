//! Capability-relative private text files. cap-std confines symlink resolution and
//! directory traversal to the opened owner directory, including concurrent renames.
//! Writes replace atomically; failed/unknown writes are never replayed by this host.
use cap_std::{ambient_authority, fs::Dir};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Clone)]
pub struct Files {
    root: PathBuf,
    gate: Arc<Mutex<()>>,
}
pub enum Request {
    List,
    Read(String),
    Write(String, String),
}
impl Files {
    pub fn new(root: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            root,
            gate: Arc::new(Mutex::new(())),
        })
    }
    pub async fn execute(&self, owner: String, request: Request) -> Result<Value, String> {
        let files = self.clone();
        tokio::task::spawn_blocking(move || files.work(&owner, request))
            .await
            .map_err(|_| "File worker stopped")?
    }
    fn work(&self, owner: &str, request: Request) -> Result<Value, String> {
        let _gate = self.gate.lock().map_err(|_| "File workspace unavailable")?;
        if owner.is_empty()
            || owner.len() > 100
            || !owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
        {
            return Err("Invalid workspace owner".into());
        }
        let root = Dir::open_ambient_dir(&self.root, ambient_authority())
            .map_err(|_| "Cannot open workspace")?;
        root.create_dir_all(owner)
            .map_err(|_| "Cannot create workspace")?;
        let dir = root
            .open_dir(owner)
            .map_err(|_| "Cannot open private workspace")?;
        match request {
            Request::List => {
                let mut files = Vec::new();
                walk(&dir, "", 0, &mut files)?;
                Ok(json!({"files":files}))
            }
            Request::Read(path) => {
                validate(&path)?;
                let metadata = dir
                    .metadata(&path)
                    .map_err(|_| "File not found or outside workspace")?;
                if !metadata.is_file() || metadata.len() > 64 * 1024 {
                    return Err("Only text files up to 64 KiB can be read".into());
                }
                let file = dir.open(&path).map_err(|_| "Cannot read file")?;
                let mut bytes = Vec::new();
                file.take(64 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Cannot read file")?;
                if bytes.len() > 64 * 1024 {
                    return Err("File exceeds 64 KiB".into());
                }
                let text = String::from_utf8(bytes).map_err(|_| "File is not UTF-8 text")?;
                Ok(json!({"path":path,"content":text}))
            }
            Request::Write(path, content) => {
                validate(&path)?;
                if content.len() > 64 * 1024 {
                    return Err("File exceeds 64 KiB".into());
                }
                let mut files = Vec::new();
                walk(&dir, "", 0, &mut files)?;
                if files.len() >= 200 && !dir.exists(&path) {
                    return Err("Workspace has reached its 200-file limit".into());
                }
                let path = Path::new(&path);
                let parent = path.parent().unwrap_or(Path::new(""));
                if !parent.as_os_str().is_empty() {
                    dir.create_dir_all(parent)
                        .map_err(|_| "Cannot create file directory")?;
                }
                let temporary = parent.join(format!(".chatty-{}.tmp", uuid::Uuid::new_v4()));
                let mut options = cap_std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                let result = (|| -> std::io::Result<()> {
                    let mut file = dir.open_with(&temporary, &options)?;
                    file.write_all(content.as_bytes())?;
                    file.sync_all()?;
                    dir.rename(&temporary, &dir, path)?;
                    Ok(())
                })();
                if result.is_err() {
                    let _ = dir.remove_file(&temporary);
                    return Err("File write failed".into());
                }
                Ok(json!({"path":path.to_string_lossy(),"bytes":content.len(),"written":true}))
            }
        }
    }
}
fn validate(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.len() > 500
        || path.contains(['\\', '\0'])
        || Path::new(path).is_absolute()
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.starts_with(".chatty-"))
        || path.split('/').count() > 8
    {
        return Err("Use a relative workspace path with at most eight components".into());
    }
    Ok(())
}
fn walk(dir: &Dir, prefix: &str, depth: usize, files: &mut Vec<Value>) -> Result<(), String> {
    if depth > 8 {
        return Err("Workspace directory nesting limit reached".into());
    }
    for entry in dir.entries().map_err(|_| "Cannot list workspace")? {
        let entry = entry.map_err(|_| "Cannot list workspace")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".chatty-") {
            continue;
        }
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = entry
            .file_type()
            .map_err(|_| "Cannot inspect workspace entry")?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            walk(
                &dir.open_dir(&name)
                    .map_err(|_| "Cannot open workspace folder")?,
                &path,
                depth + 1,
                files,
            )?;
        } else if kind.is_file() {
            files.push(json!({"path":path,"bytes":entry.metadata().map_err(|_|"Cannot inspect file")?.len()}));
            if files.len() > 200 {
                return Err("Workspace file limit reached".into());
            }
        }
    }
    files.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(())
}
