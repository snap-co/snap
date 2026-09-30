//! Compiler/filesystem contracts run explicitly, outside the fast behavior gate.
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn manifest(name: &str) -> String {
    format!(
        "[package]\nname='{name}'\nversion='0.0.0'\nedition='2024'\n[package.metadata.snap]\nrole='composition'\n"
    )
}
fn scratch() -> tempfile::TempDir {
    let cache =
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/coding-agents");
    fs::create_dir_all(&cache).unwrap();
    tempfile::Builder::new()
        .prefix("snap-static-contract-")
        .tempdir_in(cache)
        .unwrap()
}
fn package(root: &Path, name: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.toml"), manifest(name)).unwrap();
    fs::write(root.join("src/lib.rs"),"#[test]\nfn execution_is_forbidden() {\n    std::fs::write(\"test-ran\", \"bad\").unwrap();\n    panic!(\"check must not execute tests\");\n}\n").unwrap();
    fs::write(root.join("src/main.rs"),"fn main() {\n    std::fs::write(\"app-ran\", \"bad\").unwrap();\n    panic!(\"check must not start the app\");\n}\n").unwrap();
}
fn check(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_snap"))
        .arg("check")
        .args(args)
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env_remove("SNAP_MASTER_KEY")
        .output()
        .unwrap()
}
fn passed(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "static compiler/filesystem gate"]
fn app_checks_are_static_and_root_checks_select_every_workspace_member() {
    let directory = scratch();
    let root = directory.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=['app','other']\ndefault-members=['app']\nexclude=['app/external']\nresolver='3'\n",
    )
    .unwrap();
    let app = root.join("app");
    let other = root.join("other");
    package(&app, "check-app");
    package(&other, "check-other");
    let external = app.join("external");
    package(&external, "check-external");
    fs::write(
        external.join("Cargo.toml"),
        format!("{}\n[workspace]\n", manifest("check-external")),
    )
    .unwrap();
    fs::write(
        app.join("snap.toml"),
        "version=1\napplication='fixture'\n[check]\nrust=['Cargo.toml','external/Cargo.toml']\n",
    )
    .unwrap();
    // Application discovery still wins inside a nested source directory.
    passed(&check(&app.join("src"), &[]));
    assert!(!app.join("test-ran").exists());
    assert!(!app.join("app-ran").exists());
    assert!(!app.join("dist").exists());
    assert!(!app.join(".snap").exists());
    passed(&check(root, &[]));
    assert!(!root.join("test-ran").exists());
    assert!(!root.join("app-ran").exists());
    fs::write(
        other.join("src/lib.rs"),
        "pub fn invalid() -> bool {\n    \"wrong type\"\n}\n",
    )
    .unwrap();
    passed(&check(&app, &[]));
    for (where_, args) in [(root, vec![]), (app.as_path(), vec!["--workspace"])] {
        let output = check(where_, &args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("mismatched types"));
    }
    fs::write(
        other.join("src/lib.rs"),
        "pub fn valid() -> bool {\n    true\n}\n",
    )
    .unwrap();
    fs::write(
        external.join("src/lib.rs"),
        "pub fn invalid() -> bool {\n    \"wrong type\"\n}\n",
    )
    .unwrap();
    for where_ in [root, app.as_path()] {
        let output = check(where_, &[]);
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("mismatched types"),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::write(
        external.join("src/lib.rs"),
        "pub fn valid() -> bool {\n    true\n}\n",
    )
    .unwrap();
    fs::write(app.join("snap.toml"),"version=1\napplication='fixture'\n[check]\nrust=['Cargo.toml','external/Cargo.toml']\ncommands=[['sh','-c','exit 43']]\n").unwrap();
    // A nested standalone workspace must still run its enclosing app's checks.
    let output = check(&external.join("src"), &[]);
    assert!(
        !output.status.success(),
        "Nested workspace bypassed the app's static command"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("process exited with status 43"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(app.join("snap.toml"), "invalid TOML [").unwrap();
    let output = check(&external.join("src"), &["--structure-only"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("snap.toml"));
}

#[test]
#[ignore = "static compiler/filesystem gate"]
fn declared_child_manifests_replace_the_app_root_and_discover_parent_app_checks() {
    let directory = scratch();
    let workspace = directory.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let root = workspace.as_path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=['app/native']\nresolver='3'\n",
    )
    .unwrap();
    let app = root.join("app");
    package(&app.join("native"), "check-child");
    let config = "version=1\napplication='fixture'\n[check]\nrust=['native/Cargo.toml']\n";
    fs::write(app.join("snap.toml"), config).unwrap();
    // Explicit declarations need no undeclared app-root Cargo.toml.
    passed(&check(&app, &[]));
    passed(&check(&app, &["--structure-only"]));
    let external = directory.path().join("external-app");
    package(&external, "check-external-member");
    fs::write(
        external.join("Cargo.toml"),
        manifest("check-external-member")
            .replace("edition='2024'", "edition='2024'\nworkspace='../workspace'"),
    )
    .unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers=['app/native','../external-app']\nresolver='3'\n",
    )
    .unwrap();
    fs::write(
        external.join("snap.toml"),
        "version=1\napplication='external-fixture'\n[check]\ncommands=[['sh','-c','exit 44']]\n",
    )
    .unwrap();
    // Cargo membership is not limited to the workspace's filesystem subtree.
    for (where_, args) in [
        (external.as_path(), vec![]),
        (root, vec![]),
        (app.as_path(), vec!["--workspace"]),
    ] {
        let output = check(where_, &args);
        assert!(
            !output.status.success(),
            "Workspace skipped the out-of-tree app's static command"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("process exited with status 44"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::write(
        external.join("snap.toml"),
        "version=1\napplication='external-fixture'\n",
    )
    .unwrap();
    fs::write(
        app.join("snap.toml"),
        format!("{config}commands=[['sh','-c','exit 43']]\n"),
    )
    .unwrap();
    for (where_, args) in [(root, vec![]), (app.as_path(), vec!["--workspace"])] {
        let output = check(where_, &args);
        assert!(
            !output.status.success(),
            "Workspace skipped the parent app's static command"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("process exited with status 43"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[ignore = "static compiler/filesystem gate"]
fn feature_checks_enforce_only_the_selected_normal_local_dependency_graph() {
    let directory = scratch();
    let root = directory.path();
    package(root, "check-variants");
    let dependency = root.join("dependency");
    package(&dependency, "fixture-dependency");
    fs::write(root.join("Cargo.toml"),format!("{}\n[workspace]\n[features]\nstore=['dep:fixture-dependency']\n[dependencies]\nfixture-dependency={{path='dependency',optional=true}}\n",manifest("check-variants"))).unwrap();
    let config = "version=1\napplication='fixture'\n[[check.variants]]\nmanifest='Cargo.toml'\ndefault_features=false\nfeatures=[]\nallowed_local_dependencies=[]\n";
    fs::write(root.join("snap.toml"), config).unwrap();
    passed(&check(root, &[]));
    fs::write(
        root.join("snap.toml"),
        config.replace("features=[]", "features=['store']"),
    )
    .unwrap();
    let output = check(root, &[]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("unexpected normal local dependency fixture-dependency")
    );
    fs::write(
        root.join("snap.toml"),
        config.replace("features=[]", "features=['store']").replace(
            "allowed_local_dependencies=[]",
            "allowed_local_dependencies=['fixture-dependency']",
        ),
    )
    .unwrap();
    passed(&check(root, &[]));
}

#[test]
#[ignore = "static compiler/filesystem gate"]
fn declared_typescript_checks_fail_without_compiler_or_on_type_errors_without_building() {
    let directory = scratch();
    let root = directory.path();
    package(root, "check-typescript");
    fs::write(
        root.join("Cargo.toml"),
        format!("{}\n[workspace]\n", manifest("check-typescript")),
    )
    .unwrap();
    fs::write(
        root.join("snap.toml"),
        "version=1\napplication='fixture'\n[check]\ntypescript=['tsconfig.json']\n",
    )
    .unwrap();
    fs::write(
        root.join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true,"types":[],"lib":["ES2022"]},"files":["source.ts"]}"#,
    )
    .unwrap();
    fs::write(
        root.join("source.ts"),
        "const answer: number = 42; throw new Error('do not execute application code');\n",
    )
    .unwrap();
    let output = check(root, &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("TypeScript is not installed"));
    let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::os::unix::fs::symlink(
        checkout.join("node_modules").canonicalize().unwrap(),
        root.join("node_modules"),
    )
    .unwrap();
    passed(&check(root, &[]));
    fs::write(root.join("source.ts"), "const answer: number = 'wrong';\n").unwrap();
    for args in [vec![], vec!["--workspace"]] {
        let output = check(root, &args);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("TS2322"));
    }
    fs::write(
        root.join("source.ts"),
        "import type { Client } from './generated/bindings';\nexport type Binding = Client;\n",
    )
    .unwrap();
    let output = check(root, &[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("TS2307"));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Checks do not prepare generated bindings")
    );
    assert!(!root.join("generated").exists());
    assert!(!root.join("source.js").exists());
    assert!(!root.join("dist").exists());
    assert!(!root.join(".snap").exists());
}
