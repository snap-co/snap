//! Real command routing, shared build reuse and process-group retirement. External
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
    process::Command,
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
    for name in ["crates", "bin", "apps"] {
        fs::create_dir(path.join(name)).unwrap();
    }
    fs::create_dir_all(path.join("crates/store")).unwrap();
    fs::write(path.join("Cargo.toml"), "[workspace]\nmembers=['crates/core','apps/example']\n[workspace.dependencies]\nexample={path='apps/example'}\n").unwrap();
    for (directory, name, role, source) in [
        (
            "crates/core",
            "snap-core-properties",
            "tool",
            "#[test]\nfn formats_integer() { let input = std::fs::read_to_string(\"input.txt\").unwrap(); assert_eq!(itoa::Buffer::new().format(input.trim().parse::<u32>().unwrap()), \"123\"); assert_eq!(std::env::var(\"CARGO_MANIFEST_DIR\").unwrap(), env!(\"CARGO_MANIFEST_DIR\")); }\n",
        ),
        (
            "apps/example",
            "fixture-app",
            "application",
            "compile_error!(\"framework verification must not compile this app\");\n",
        ),
    ] {
        let package = path.join(directory);
        fs::create_dir_all(package.join("src")).unwrap();
        fs::write(package.join("Cargo.toml"), format!("[package]\nname='{name}'\nversion='0.0.0'\nedition='2024'\n[package.metadata.snap]\nrole='{role}'\n{}", if role == "tool" { "[dependencies]\nitoa='1'\n" } else { "" })).unwrap();
        fs::write(package.join("src/lib.rs"), source).unwrap();
        fs::write(package.join("input.txt"), "123\n").unwrap();
    }
    fs::write(path.join("metadata.json"), serde_json::json!({
        "workspace_root": path,
        "target_directory": path.join("target"),
        "workspace_members": ["core", "app", "store"],
        "packages": [
            {"id":"core", "name":"snap-core-properties", "manifest_path":path.join("crates/core/Cargo.toml")},
            {"id":"app", "name":"fixture-app", "manifest_path":path.join("apps/example/Cargo.toml")},
            {"id":"store", "name":"snap-store", "manifest_path":path.join("crates/store/Cargo.toml")},
        ],
    }).to_string()).unwrap();
}
fn verifier(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_snap"));
    command
        .args(["verify-framework", "--root"])
        .arg(root)
        .env("VERIFY_ROOT", root)
        .env(
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
        line.strip_prefix("Cold build artifacts: ")
            .map(PathBuf::from)
    })
}

#[test]
fn framework_help_explains_selection_without_running_tools() {
    let directory = scratch();
    for flag in ["--help", "-h"] {
        let output = Command::new(env!("CARGO_BIN_EXE_snap"))
            .args(["verify-framework", flag])
            .current_dir(directory.path())
            .env("PATH", directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("./bin/test [OPTIONS] [GATE]"));
        assert!(text.contains("--matrix"));
        assert!(text.contains("--timeout"));
        assert!(text.contains("fast + full"));
        assert!(text.contains("full only"));
        assert!(text.contains("SQLite in memory"));
        assert!(text.contains("WebSocket loopback"));
        assert!(text.contains("summary.json"));
        assert!(text.contains("SNAP_BROWSER"));
    }
}

#[test]
fn framework_selection_runs_in_place_and_rejects_empty_gates() {
    let directory = scratch();
    let path = directory.path();
    root(path);
    executable(
        &path.join("bin/test-contract"),
        r#"
if [ "$1" = --list ]; then
  if [ "${2:-}" != --ignored ]; then printf '%s\n' 'contract: test' 'another_contract: test'; fi
  exit
fi
printf '%s\n' 'observable failure detail'
case "$VERIFY_RESULT" in
  pass) echo 'test result: ok. 2 passed; 0 failed; 0 ignored';;
  partial) echo 'test result: ok. 1 passed; 0 failed; 0 ignored';;
  empty) echo 'test result: ok. 0 passed; 0 failed; 0 ignored';;
  fail) exit 43;;
esac
"#,
    );
    executable(
        &path.join("bin/cargo"),
        r#"
test "$PWD" = "$VERIFY_ROOT"
test -d apps
if [ "$1" = metadata ]; then cat metadata.json; exit; fi
previous=''
selected=''
for arg in "$@"; do
  test "$arg" != --workspace
  if [ "$previous" = -p ]; then test "$arg" = snap-store; selected=yes; fi
  previous="$arg"
done
test "$selected" = yes
printf '{"reason":"compiler-artifact","package_id":"store","profile":{"test":true},"target":{"name":"contracts"},"executable":"%s/bin/test-contract"}\n' "$VERIFY_ROOT"
"#,
    );
    for (result, success) in [
        ("pass", true),
        ("partial", false),
        ("empty", false),
        ("fail", false),
    ] {
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
        assert_eq!(
            format!("{text}{}", String::from_utf8_lossy(&output.stderr))
                .contains("observable failure detail"),
            !success
        );
        let logs = text
            .lines()
            .find_map(|line| line.strip_prefix("Logs: "))
            .unwrap();
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(Path::new(logs).join("summary.json")).unwrap())
                .unwrap();
        assert!(
            summary["outcomes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|outcome| outcome["module"] == "Store"
                    && outcome["selected_cases"] == 2
                    && outcome["status"] == if success { "PASS" } else { "FAIL" })
        );
        assert!(
            fs::read_dir(logs)
                .unwrap()
                .filter_map(Result::ok)
                .any(|entry| {
                    entry.file_name().to_string_lossy().ends_with("-list.out")
                        && fs::read_to_string(entry.path())
                            .unwrap()
                            .contains("another_contract: test")
                }),
            "normal case enumeration must not be overwritten by the ignored listing"
        );
        assert!(retained(&text).is_none());
        assert!(
            fs::read_to_string(path.join("Cargo.toml"))
                .unwrap()
                .contains("apps/example")
        );
    }
}

#[test]
#[ignore = "compiler/filesystem cache contract"]
fn framework_reuses_workspace_artifacts_and_cold_runs_leave_them_intact() {
    let directory = scratch();
    let path = directory.path();
    root(path);
    let target = path.join("shared-target");
    let normal = Command::new(env!("CARGO"))
        .current_dir(path)
        .args(["test", "--offline", "-p", "snap-core-properties"])
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .output()
        .unwrap();
    assert!(
        normal.status.success(),
        "{}",
        String::from_utf8_lossy(&normal.stderr)
    );
    executable(
        &path.join("bin/cargo"),
        r#"
if [ "$1" = test ]; then
  shift
  exec "$VERIFY_REAL_CARGO" test --offline "$@"
fi
exec "$VERIFY_REAL_CARGO" "$@" --offline
"#,
    );
    for (cold, fresh) in [(false, true), (false, true), (true, false), (false, true)] {
        let mut command = verifier(path);
        command
            .arg("properties")
            .arg("--verbose")
            .env("VERIFY_REAL_CARGO", env!("CARGO"))
            .env("CARGO_TARGET_DIR", &target)
            .env_remove("CARGO_BUILD_BUILD_DIR");
        if cold {
            command
                .arg("--cold")
                .env("CARGO_BUILD_BUILD_DIR", path.join("inherited-build-dir"));
        }
        let output = command.output().unwrap();
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success(),
            "{text}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        for name in ["itoa", "snap_core_properties"] {
            let artifacts: Vec<_> = text
                .lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter(|value| {
                    value["reason"] == "compiler-artifact" && value["target"]["name"] == name
                })
                .collect();
            assert!(!artifacts.is_empty(), "Cargo must report {name}: {text}");
            for artifact in artifacts {
                assert_eq!(artifact["fresh"], fresh, "cold={cold}: {text}");
                let file = Path::new(artifact["filenames"][0].as_str().unwrap());
                assert_eq!(file.exists(), !cold, "{}", file.display());
            }
        }
        if let Some(cold_target) = retained(&text) {
            assert!(!cold_target.exists());
        }
    }
}

#[test]
fn layer_report_runs_host_jobs_in_parallel_and_distinguishes_capability_exclusions() {
    let directory = scratch();
    let path = directory.path();
    root(path);
    let mut metadata: serde_json::Value =
        serde_json::from_slice(&fs::read(path.join("metadata.json")).unwrap()).unwrap();
    metadata["packages"][2]["name"] = "snap-platform-tests".into();
    fs::create_dir_all(path.join("crates/sqlite")).unwrap();
    metadata["packages"].as_array_mut().unwrap().push(serde_json::json!({"id":"sqlite","name":"snap-store-sqlite","manifest_path":path.join("crates/sqlite/Cargo.toml")}));
    metadata["workspace_members"]
        .as_array_mut()
        .unwrap()
        .push("sqlite".into());
    fs::write(path.join("metadata.json"), metadata.to_string()).unwrap();
    executable(
        &path.join("bin/matrix-contract"),
        r#"
if [ "$1" = --list ]; then
  if [ "${2:-}" != --ignored ]; then
    echo 'memory::controlled::journey: test'
    echo 'sqlite_memory::tcp::journey: test'
    echo 'sqlite_file::websocket::journey: test'
  fi
  exit
fi
touch "$VERIFY_ROOT/started-$1"
# Both configurations must start before either can complete. Serial execution
# fails with a bounded diagnostic instead of hanging the test runner.
for attempt in $(seq 1 200); do
  if [ -e "$VERIFY_ROOT/started-memory::controlled::journey" ] && [ -e "$VERIFY_ROOT/started-sqlite_memory::tcp::journey" ]; then
    echo 'successful child chatter belongs in logs'
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored'
    exit
  fi
  sleep 0.01
done
echo 'configuration jobs were serialized' >&2
exit 19
"#,
    );
    executable(
        &path.join("bin/durability-contract"),
        r#"
if [ "$1" = --list ]; then echo 'abrupt_process_exit_preserves_commits: test'; exit; fi
echo 'test result: ok. 1 passed; 0 failed; 0 ignored'
"#,
    );
    executable(
        &path.join("bin/cargo"),
        r#"
test "$PWD" = "$VERIFY_ROOT"
if [ "$1" = metadata ]; then cat metadata.json; exit; fi
printf '{"reason":"compiler-artifact","package_id":"store","profile":{"test":true},"target":{"name":"matrix"},"executable":"%s/bin/matrix-contract"}\n' "$VERIFY_ROOT"
printf '{"reason":"compiler-artifact","package_id":"sqlite","profile":{"test":true},"target":{"name":"recovery"},"executable":"%s/bin/durability-contract"}\n' "$VERIFY_ROOT"
echo 'warning: a dependency warning' >&2
echo 'warning: a dependency warning' >&2
"#,
    );
    for (matrix, configurations) in [("fast", 2), ("full", 3)] {
        let output = verifier(path)
            .args(["interface", "--matrix", matrix, "--jobs", "2"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{text}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains("successful child chatter"));
        assert_eq!(text.matches("warning: a dependency warning").count(), 1);
        let logs = text
            .lines()
            .find_map(|line| line.strip_prefix("Logs: "))
            .unwrap();
        let summary: serde_json::Value =
            serde_json::from_slice(&fs::read(Path::new(logs).join("summary.json")).unwrap())
                .unwrap();
        let outcomes = summary["outcomes"].as_array().unwrap();
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome["module"] == "Transport + Store"
                    && outcome["status"] == "PASS")
                .count(),
            configurations
        );
        assert_eq!(
            outcomes.iter().any(|outcome| {
                outcome["configuration"]
                    .as_str()
                    .unwrap()
                    .contains("SQLite file / WebSocket")
                    && outcome["status"]
                        .as_str()
                        .unwrap()
                        .starts_with("NOT SELECTED")
            }),
            matrix == "fast"
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| outcome["contract"] == "Process-crash durability"
                    && outcome["status"].as_str().unwrap().starts_with("N/A"))
                .count(),
            2
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome["contract"] == "Process-crash durability"
                    && outcome["status"] == "PASS")
        );
        assert_eq!(
            path.join("started-sqlite_file::websocket::journey")
                .exists(),
            matrix == "full"
        );
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
fn cancellation_and_deadlines_retire_metadata_build_and_contract_descendants() {
    for (phase, signal, resistant) in [
        ("metadata", Some(Signal::SIGTERM), false),
        ("gate", Some(Signal::SIGTERM), false),
        ("gate", Some(Signal::SIGINT), true),
        ("execution", Some(Signal::SIGINT), true),
        ("execution", None, true),
    ] {
        let directory = scratch();
        let path = directory.path();
        root(path);
        let ready = path.join("ready");
        let log = path.join("output");
        executable(
            &path.join("bin/cargo"),
            r#"
if [ "$1" = metadata ] && [ "$VERIFY_PHASE" != metadata ]; then cat "$VERIFY_ROOT/metadata.json"; exit; fi
if [ "$VERIFY_PHASE" = execution ]; then
  printf '{"reason":"compiler-artifact","package_id":"store","profile":{"test":true},"target":{"name":"contracts"},"executable":"%s/bin/long-contract"}\n' "$VERIFY_ROOT"
  exit
fi
if [ "$VERIFY_RESISTANT" = yes ]; then
  (trap '' INT TERM; exec "$VERIFY_HELPER" --exact fixture_child --ignored --nocapture) &
else
  "$VERIFY_HELPER" --exact fixture_child --ignored --nocapture &
fi
wait
"#,
        );
        executable(
            &path.join("bin/long-contract"),
            r#"
if [ "$1" = --list ]; then echo 'listener_contract: test'; exit; fi
trap "echo 'test result: ok. 1 passed; 0 failed; 0 ignored'; exit 0" TERM
(trap '' INT TERM; exec "$VERIFY_HELPER" --exact fixture_child --ignored --nocapture) &
wait
"#,
        );
        let mut command = verifier(path);
        command.arg("io");
        if signal.is_none() {
            command.args(["--timeout", "1"]);
        }
        let mut child = command
            .env("VERIFY_READY", &ready)
            .env("VERIFY_PHASE", phase)
            .env("VERIFY_RESISTANT", if resistant { "yes" } else { "no" })
            .env("VERIFY_HELPER", std::env::current_exe().unwrap())
            .stdout(fs::File::create(&log).unwrap())
            .stderr(fs::File::create(log.with_extension("err")).unwrap())
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
        if let Some(signal) = signal {
            kill(Pid::from_raw(child.id() as i32), signal).unwrap();
        }
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
        assert_eq!(
            status.code(),
            Some(signal.map_or(1, |signal| 128 + signal as i32))
        );
        if signal.is_none() {
            assert!(
                fs::read_to_string(log.with_extension("err"))
                    .unwrap()
                    .contains("execution deadline")
            );
        }
        assert!(
            TcpStream::connect(("127.0.0.1", port)).is_err(),
            "descendant listener survived"
        );
        if let Some(copy) = retained(&fs::read_to_string(log).unwrap()) {
            fs::remove_dir_all(copy).unwrap();
        }
    }
}
