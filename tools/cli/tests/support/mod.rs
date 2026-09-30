use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
pub mod command;

pub const CONFIG: &str = "version=1\napplication='fixture'\n";
pub const MANIFEST: &str =
    "[package]\nname='cli-fixture'\nversion='0.0.0'\nedition='2024'\n[workspace]\n";

pub struct Project {
    _directory: tempfile::TempDir,
    pub root: PathBuf,
    pub helper: PathBuf,
}

impl Project {
    pub fn new(role: Option<&str>) -> Self {
        // These fixtures compile code; keep sources and targets on disk, not /tmp.
        let cache = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/coding-agents");
        fs::create_dir_all(&cache).unwrap();
        let directory = tempfile::Builder::new()
            .prefix("snap-cli-contract-")
            .tempdir_in(cache)
            .unwrap();
        let root = directory.path().join("project");
        fs::create_dir_all(root.join("src")).unwrap();
        let manifest = match role {
            Some(role) => format!("{MANIFEST}\n[package.metadata.snap]\nrole='{role}'\n"),
            None => format!("{MANIFEST}\n[package.metadata.snap]\nrole='composition'\n"),
        };
        fs::write(root.join("Cargo.toml"), manifest).unwrap();
        fs::write(root.join("src/lib.rs"), "#![no_std]\n").unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    std::process::exit(37);\n}\n",
        )
        .unwrap();
        fs::write(root.join("snap.toml"), CONFIG).unwrap();
        let helper = std::env::current_exe().unwrap();
        Self {
            _directory: directory,
            root,
            helper,
        }
    }

    pub fn command(&self, cwd: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_snap"));
        command
            .current_dir(cwd)
            .env("CARGO_TARGET_DIR", self.root.join("target"));
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command(&self.root).args(args).output().unwrap()
    }

    pub fn declaration(&self, args: &[&str]) -> String {
        let mut argv = vec![self.helper.to_string_lossy().into_owned()];
        argv.extend(["--exact", "fixture_child", "--ignored", "--nocapture"].map(str::to_owned));
        // libtest accepts literal values after --skip. Only the selected helper
        // reads these values; there is no shell expansion or production hook.
        for arg in args {
            argv.extend(["--skip".to_owned(), (*arg).to_owned()]);
        }
        serde_json::to_string(&vec![argv]).unwrap()
    }

    pub fn config(&self, extra: &str) {
        fs::write(self.root.join("snap.toml"), format!("{CONFIG}{extra}")).unwrap();
    }

    pub fn dependency(&self, name: &str, role: &str, source: &str) {
        let path = self.root.join(name);
        fs::create_dir_all(path.join("src")).unwrap();
        fs::write(path.join("Cargo.toml"), format!(
            "[package]\nname='{name}'\nversion='0.0.0'\nedition='2024'\n[package.metadata.snap]\nrole='{role}'\n"
        )).unwrap();
        fs::write(path.join("src/lib.rs"), source).unwrap();
    }
}

pub fn passed(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub fn failed(output: &Output, messages: &[&str]) {
    assert!(
        !output.status.success(),
        "Unexpected success: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for message in messages {
        assert!(stderr.contains(message), "Expected {message:?}: {stderr}");
    }
}
