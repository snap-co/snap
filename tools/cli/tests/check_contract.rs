#[path = "support/architecture.rs"]
mod architecture;

#[test]
#[ignore = "compiler/filesystem dependency-tool contract"]
fn dependency_gate_requires_pinned_tools_and_only_selects_workspace_members() {
    let project = Project::new(None);
    let source = project.root.join("dependency_command.rs");
    fs::write(&source, include_str!("support/dependency_command.rs")).unwrap();
    let fake = project.root.join("tools");
    fs::create_dir(&fake).unwrap();
    passed(
        &std::process::Command::new("rustc")
            .args(["--edition=2024", "-o"])
            .arg(fake.join("cargo"))
            .arg(source)
            .output()
            .unwrap(),
    );
    fs::write(
        project.root.join("mise.toml"),
        "[tools]\n'cargo:cargo-machete'='0.9.2'\n'cargo:cargo-deny'='0.20.2'\n",
    )
    .unwrap();
    fs::write(project.root.join("machete.version"), "0.9.2").unwrap();
    fs::write(project.root.join("deny.version"), "0.20.2").unwrap();
    let selected = project.root.join("Cargo.toml");
    let excluded = project.root.join("dependency/Cargo.toml");
    fs::write(
        project.root.join("metadata.json"),
        serde_json::json!({
            "workspace_root": project.root,
            "workspace_members": ["selected"],
            "packages": [
                {"id":"selected", "manifest_path":selected},
                {"id":"dependency", "manifest_path":excluded},
            ],
        })
        .to_string(),
    )
    .unwrap();
    let run = |args: &[&str]| {
        project
            .command(&project.root)
            .env("PATH", &fake)
            .args(args)
            .output()
            .unwrap()
    };
    for (args, suffix) in [
        (vec!["check-deps"], "deny\ncheck\nbans\nsources\nEND\n"),
        (
            vec!["check-deps", "--audit"],
            "deny\ncheck\nbans\nsources\nEND\ndeny\ncheck\nadvisories\nEND\n",
        ),
    ] {
        passed(&run(&args));
        assert_eq!(
            fs::read_to_string(project.root.join("invocations")).unwrap(),
            format!("machete\n{}\nEND\n{suffix}", selected.display())
        );
        fs::remove_file(project.root.join("invocations")).unwrap();
    }
    fs::write(project.root.join("machete.version"), "0.0.0").unwrap();
    failed(&run(&["check-deps"]), &["Expected cargo-machete 0.9.2"]);
    assert!(!project.root.join("invocations").exists());
    fs::write(project.root.join("mise.toml"), "[tools]\n").unwrap();
    failed(
        &run(&["check-deps"]),
        &["Missing pinned dependency tool version"],
    );
    assert!(!project.root.join("invocations").exists());
}

#[test]
#[ignore = "subprocess fixture, invoked by CLI contracts"]
fn fixture_child() {
    support::command::run();
}
mod support;

use std::{
    fs,
    net::TcpStream,
    process::Stdio,
    time::{Duration, Instant},
};
use support::{CONFIG, Project, failed, passed};

#[test]
#[ignore = "compiler/filesystem contract"]
fn failures_and_missing_tools_cannot_report_success() {
    let project = Project::new(None);
    for (first, code, message) in [
        (
            project.declaration(&["exit", "43"]),
            43,
            "Project check failed",
        ),
        (
            "[[\"snap-check-missing-tool\"]]".into(),
            1,
            "Install the command",
        ),
    ] {
        let mut commands: serde_json::Value = serde_json::from_str(&first).unwrap();
        commands.as_array_mut().unwrap().push(
            serde_json::from_str::<serde_json::Value>(&project.declaration(&["record", "never"]))
                .unwrap()[0]
                .clone(),
        );
        project.config(&format!("[check]\ncommands={commands}\n"));
        let output = project
            .command(&project.root.join("src"))
            .arg("check")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        failed(&output, &[message]);
        assert!(!project.root.join("ran").exists());
        assert!(!project.root.join(".snap/build/debug").exists());
    }
    fs::write(
        project.root.join("src/main.rs"),
        "fn main() {\n    missing();\n}\n",
    )
    .unwrap();
    failed(&project.run(&["check"]), &["missing"]);
    assert!(!project.root.join("ran").exists());
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn sibling_checks_do_not_inherit_another_projects_commands() {
    let project = Project::new(None);
    let sibling = project.root.join("sibling");
    fs::create_dir_all(sibling.join("src")).unwrap();
    fs::copy(project.root.join("Cargo.toml"), sibling.join("Cargo.toml")).unwrap();
    fs::write(sibling.join("src/main.rs"), "fn main() {}\n").unwrap();
    project.config(&format!(
        "[check]\ncommands={}\n",
        project.declaration(&["exit", "44"])
    ));
    fs::write(
        sibling.join("snap.toml"),
        format!(
            "{CONFIG}[check]\ncommands={}\n",
            project.declaration(&["print", "12345"])
        ),
    )
    .unwrap();
    let output = project.run(&["check", "sibling"]);
    passed(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("12345"));
    fs::write(sibling.join("snap.toml"), "invalid config").unwrap();
    failed(
        &project
            .command(&sibling.join("src"))
            .arg("check")
            .output()
            .unwrap(),
        &[sibling.join("snap.toml").to_str().unwrap()],
    );
}

#[test]
#[ignore = "compiler/process contract"]
fn interrupt_check_cleans_its_listener_descendant() {
    use nix::{
        sys::signal::{Signal, kill, killpg},
        unistd::Pid,
    };
    use std::os::unix::process::CommandExt;
    let project = Project::new(None);
    project.config(&format!(
        "[check]\ncommands={}\n",
        project.declaration(&["descendant"])
    ));
    let child = project
        .command(&project.root)
        .arg("check")
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    struct Cleanup {
        child: std::process::Child,
        descendant: Option<Pid>,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = kill(Pid::from_raw(self.child.id() as i32), Signal::SIGTERM);
            if let Some(pid) = self.descendant {
                let _ = killpg(pid, Signal::SIGKILL);
                let _ = kill(pid, Signal::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = project.root.join("ready");
    let mut guard = Cleanup {
        child,
        descendant: None,
    };
    while !ready.exists() {
        assert!(
            guard.child.try_wait().unwrap().is_none(),
            "CLI exited before check became ready"
        );
        assert!(Instant::now() < deadline, "Check never became ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    let ready = fs::read_to_string(ready).unwrap();
    let values: Vec<u32> = ready
        .split_whitespace()
        .map(|value| value.parse().unwrap())
        .collect();
    guard.descendant = Some(Pid::from_raw(values[1] as i32));
    // Prove that ordinary graceful termination cannot satisfy the port assertion.
    kill(guard.descendant.unwrap(), Signal::SIGTERM).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, values[0] as u16)).is_ok(),
        "Listener did not survive graceful termination"
    );
    kill(Pid::from_raw(guard.child.id() as i32), Signal::SIGTERM).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = guard.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "CLI failed to stop");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(143));
    assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, values[0] as u16)).is_err());
}

#[test]
#[ignore = "filesystem/command contract"]
fn native_suite_runs_literal_commands_without_inherited_artifacts() {
    let project = Project::new(None);
    let argv = ["a b", "", "*", "$(touch bad)"];
    let mut args = vec!["literal"];
    args.extend(argv);
    project.config(&format!(
        "[test.native]\ncommands={}\n",
        project.declaration(&args)
    ));
    let output = project
        .command(&project.root)
        .args(["test", project.root.to_str().unwrap(), "native"])
        .env("SNAP_CHECK_EXECUTABLE", "/other/app")
        .env("SNAP_CHECK_PACKAGE", "/other/package")
        .env("SNAP_CHECK_WEB_DIR", "/other/web")
        .output()
        .unwrap();
    passed(&output);
    assert_eq!(
        fs::read_to_string(project.root.join("cwd")).unwrap(),
        project.root.to_str().unwrap()
    );
    assert_eq!(
        fs::read(project.root.join("argv")).unwrap(),
        argv.join("\0")
            .into_bytes()
            .into_iter()
            .chain([0])
            .collect::<Vec<_>>()
    );
    assert!(!project.root.join("bad").exists());
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn platform_selection_defaults_isolates_and_full_runs_all() {
    let project = Project::new(None);
    let mut config = format!(
        "[check]\ncommands={}\n",
        project.declaration(&["record", "check"])
    );
    for name in ["memory", "native", "workers", "browser", "full"] {
        config.push_str(&format!(
            "[test.{name}]\ncommands={}\n",
            project.declaration(&["record", name])
        ));
    }
    project.config(&config);
    for (args, expected) in [
        (vec!["test"], "memory:memory\n"),
        (vec!["test", "native"], "native:native\n"),
        (
            vec!["test", project.root.to_str().unwrap(), "workers"],
            "workers:workers\n",
        ),
        (vec!["check"], "check:check\n"),
        (
            vec!["test", "full"],
            "check:check\nmemory:memory\nnative:native\nworkers:workers\nbrowser:browser\nfull:full\n",
        ),
    ] {
        passed(&project.run(&args));
        assert_eq!(
            fs::read_to_string(project.root.join("ran")).unwrap(),
            expected
        );
        fs::remove_file(project.root.join("ran")).unwrap();
    }
    failed(
        &project.run(&["test", "wasm"]),
        &["select workers or browser"],
    );
}

#[test]
#[ignore = "filesystem/command contract"]
fn unsupported_empty_and_failed_suites_do_not_pass() {
    let project = Project::new(None);
    project.config(&format!(
        "[test.memory]\ncommands={}\n",
        project.declaration(&["exit", "43"])
    ));
    failed(&project.run(&["test", "native"]), &["No native test suite"]);
    assert_eq!(project.run(&["test"]).status.code(), Some(43));
    project.config("[test.memory]\ncommands=[]\n");
    failed(&project.run(&["test"]), &["must not be empty"]);
}

#[test]
#[ignore = "filesystem/configuration contract"]
fn removed_host_workflow_is_rejected() {
    let project = Project::new(None);
    for section in [
        "[server]\nmanifest='Cargo.toml'\nbin='cli-fixture'",
        "[web]\napplication='app.ts'",
        "[prepare]\nbuild=[['true']]",
        "[dev]\naddress='127.0.0.1:0'",
        "[test.native]\nbuild=true\ncommands=[['true']]",
    ] {
        project.config(section);
        failed(&project.run(&["check"]), &["unknown field"]);
    }
}
