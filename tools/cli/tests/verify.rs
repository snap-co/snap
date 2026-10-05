//! Real command routing, source isolation and process-group retirement. External
//! command fixtures avoid compiling a second toolchain inside these contracts.
use nix::{
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use std::{
    fs,
    io::Write,
    net::{TcpListener, TcpStream},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn executable(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/bash\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn scratch() -> tempfile::TempDir {
    let cache = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/coding-agents");
    fs::create_dir_all(&cache).unwrap();
    tempfile::Builder::new()
        .prefix("snap-verifier-test-")
        .tempdir_in(cache)
        .unwrap()
}
fn root(path: &Path) {
    for name in ["crates", "kits", "tools", "tests", "bin", "apps"] {
        fs::create_dir(path.join(name)).unwrap();
    }
    for name in [
        "Cargo.lock",
        "mise.toml",
        "deny.toml",
        "bun.lock",
        "tsconfig.json",
    ] {
        fs::write(path.join(name), "").unwrap();
    }
    fs::write(path.join("package.json"), r#"{"workspaces":["apps/*"]}"#).unwrap();
    fs::write(path.join("Cargo.toml"), "[workspace]\nmembers=['crates/core','apps/example']\n[workspace.dependencies]\nexample={path='apps/example'}\n").unwrap();
}
fn verifier(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_snap"));
    command.args(["verify-framework", "--root"]).arg(root).env(
        "PATH",
        format!(
            "{}:{}",
            root.join("bin").display(),
            std::env::var("PATH").unwrap()
        ),
    );
    command
}
fn retained(output: &str) -> Option<PathBuf> {
    output.lines().find_map(|line| {
        line.strip_prefix("Isolated verification: ")
            .map(PathBuf::from)
    })
}

#[test]
fn isolated_selection_rejects_empty_gates_and_preserves_failures() {
    let directory = scratch();
    let path = directory.path();
    root(path);
    executable(
        &path.join("bin/cargo"),
        r#"
test ! -e apps
if [ "$1" = metadata ]; then echo '{"packages":[]}'; exit; fi
printf '%s\n' 'observable failure detail'
case "$VERIFY_RESULT" in
  pass) echo 'test result: ok. 1 passed; 0 failed; 0 ignored';;
  empty) echo 'test result: ok. 0 passed; 0 failed; 0 ignored';;
  fail) exit 43;;
esac
"#,
    );
    for (result, success) in [("pass", true), ("empty", false), ("fail", false)] {
        let output = verifier(path)
            .arg("test")
            .env("VERIFY_RESULT", result)
            .output()
            .unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(text.contains("observable failure detail"));
        let copy = retained(&text).unwrap();
        if success {
            assert!(!copy.exists());
        } else {
            let manifest: toml::Value =
                toml::from_str(&fs::read_to_string(copy.join("source/Cargo.toml")).unwrap())
                    .unwrap();
            assert_eq!(
                manifest["workspace"]["members"].as_array().unwrap().len(),
                1
            );
            assert!(
                manifest["workspace"]["dependencies"]
                    .as_table()
                    .unwrap()
                    .is_empty()
            );
            assert!(!copy.join("source/apps").exists());
            fs::remove_dir_all(copy).unwrap();
        }
    }
}

#[test]
fn browser_wrapper_preserves_arguments_and_both_aggregates() {
    let directory = scratch();
    let path = directory.path();
    let record = path.join("commands");
    executable(
        &path.join("mise"),
        "printf '%s\\0' \"$@\" >> \"$RECORD\"; printf '\\n' >> \"$RECORD\"; exit \"${RESULT:-0}\"",
    );
    let wrapper = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin/browser-tests");
    for (args, packages) in [
        (vec![], vec!["snap-browser-tests", "snap-app-browser-tests"]),
        (
            vec!["all"],
            vec!["snap-browser-tests", "snap-app-browser-tests"],
        ),
        (
            vec!["--browser", "chromium", "testy"],
            vec!["snap-app-browser-tests"],
        ),
        (
            vec!["--filter", "production", "factorio"],
            vec!["snap-app-browser-tests"],
        ),
        (
            vec!["--artifacts=authy", "--filter=journey", "--", "chatty"],
            vec!["snap-app-browser-tests"],
        ),
        (
            vec!["--browser", "authy", "client"],
            vec!["snap-browser-tests"],
        ),
        (
            vec!["--artifacts", "testy", "react"],
            vec!["snap-browser-tests"],
        ),
    ] {
        fs::write(&record, "").unwrap();
        assert!(
            Command::new(&wrapper)
                .args(&args)
                .env("RECORD", &record)
                .env(
                    "PATH",
                    format!("{}:{}", path.display(), std::env::var("PATH").unwrap())
                )
                .status()
                .unwrap()
                .success()
        );
        let expected: String = packages
            .iter()
            .map(|package| {
                let mut command = vec![
                    "exec",
                    "--",
                    "cargo",
                    "run",
                    "-p",
                    *package,
                    "--",
                    "--prepare",
                ];
                command.extend(&args);
                format!("{}\0\n", command.join("\0"))
            })
            .collect();
        assert_eq!(fs::read_to_string(&record).unwrap(), expected);
    }
    fs::write(&record, "").unwrap();
    let status = Command::new(wrapper)
        .arg("all")
        .env("RECORD", &record)
        .env("RESULT", "43")
        .env(
            "PATH",
            format!("{}:{}", path.display(), std::env::var("PATH").unwrap()),
        )
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(43));
    assert_eq!(fs::read_to_string(record).unwrap().lines().count(), 1);
}

#[test]
#[ignore = "subprocess listener fixture"]
fn fixture_child() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let ready = PathBuf::from(std::env::var_os("VERIFY_READY").unwrap());
    let mut file = fs::File::create(ready.with_extension("pending")).unwrap();
    writeln!(
        file,
        "{} {}",
        std::process::id(),
        listener.local_addr().unwrap().port()
    )
    .unwrap();
    fs::rename(ready.with_extension("pending"), &ready).unwrap();
    for connection in listener.incoming() {
        drop(connection.unwrap());
    }
}

#[test]
fn cancellation_retires_metadata_and_gate_descendants() {
    for (phase, signal, resistant) in [
        ("metadata", Signal::SIGTERM, false),
        ("gate", Signal::SIGTERM, false),
        ("gate", Signal::SIGINT, true),
    ] {
        let directory = scratch();
        let path = directory.path();
        root(path);
        let ready = path.join("ready");
        let log = path.join("output");
        executable(
            &path.join("bin/cargo"),
            r#"
if [ "$1" = metadata ] && [ "$VERIFY_PHASE" != metadata ]; then echo '{"packages":[]}'; exit; fi
if [ "$VERIFY_RESISTANT" = yes ]; then
  (trap '' INT TERM; exec "$VERIFY_HELPER" --exact fixture_child --ignored --nocapture) &
else
  "$VERIFY_HELPER" --exact fixture_child --ignored --nocapture &
fi
wait
"#,
        );
        let mut child = verifier(path)
            .arg("io")
            .env("VERIFY_READY", &ready)
            .env("VERIFY_PHASE", phase)
            .env("VERIFY_RESISTANT", if resistant { "yes" } else { "no" })
            .env("VERIFY_HELPER", std::env::current_exe().unwrap())
            .stdout(fs::File::create(&log).unwrap())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !ready.exists() && Instant::now() < deadline && child.try_wait().unwrap().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
        if !ready.exists() {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "listener did not start: {}",
                fs::read_to_string(log).unwrap()
            );
        }
        let address = fs::read_to_string(&ready).unwrap();
        let port: u16 = address.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok());
        kill(Pid::from_raw(child.id() as i32), signal).unwrap();
        let deadline = Instant::now() + Duration::from_secs(12);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("verifier failed to retire");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(128 + signal as i32));
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "descendant listener survived"
        );
        if let Some(copy) = retained(&fs::read_to_string(log).unwrap()) {
            fs::remove_dir_all(copy).unwrap();
        }
    }
}
