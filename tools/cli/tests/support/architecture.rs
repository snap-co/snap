use crate::support::{MANIFEST, Project, failed, passed};
use std::fs;

#[test]
#[ignore = "compiler/filesystem contract"]
fn interfaces_depend_only_on_other_interfaces() {
    let project = Project::new(Some("interface"));
    project.dependency("provider", "core", "#![no_std]\n");
    fs::write(project.root.join("Cargo.toml"), format!("{MANIFEST}[package.metadata.snap]\nrole='interface'\n[dependencies]\nprovider={{path='provider'}}\n")).unwrap();
    failed(
        &project.run(&["check", "--structure-only"]),
        &[
            "Dependency direction violations",
            "Interface",
            "Core",
            "normal",
        ],
    );
    let path = project.root.join("provider/Cargo.toml");
    fs::write(
        &path,
        fs::read_to_string(&path)
            .unwrap()
            .replace("role='core'", "role='interface'"),
    )
    .unwrap();
    passed(&project.run(&["check", "--structure-only"]));
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn explicit_rlib_is_a_portable_library() {
    let project = Project::new(Some("application"));
    let path = project.root.join("Cargo.toml");
    fs::write(
        &path,
        format!(
            "{}[lib]\ncrate-type=['rlib']\n",
            fs::read_to_string(&path).unwrap()
        ),
    )
    .unwrap();
    passed(&project.run(&["check", "--structure-only"]));
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn portable_path_dependency_features_are_validated_in_its_workspace() {
    let project = Project::new(Some("application"));
    project.dependency("external", "core", "#![no_std]\n");
    project.dependency("external/platform", "platform", "#![no_std]\n");
    fs::write(project.root.join("external/platform/Cargo.toml"), "[package]\nname='fixture-platform'\nversion='0.0.0'\nedition='2024'\n[package.metadata.snap]\nrole='platform'\n").unwrap();
    fs::write(project.root.join("Cargo.toml"), format!("{}[package.metadata.snap]\nrole='application'\n[dependencies]\nexternal={{path='external'}}\n", MANIFEST.replace("[workspace]", "[workspace]\nexclude=['external']"))).unwrap();
    for header in [
        "dependencies",
        "target.'cfg(target_os=\"none\")'.dependencies",
    ] {
        fs::write(project.root.join("external/Cargo.toml"), format!("[package]\nname='external'\nversion='0.0.0'\nedition='2024'\n[package.metadata.snap]\nrole='core'\n[workspace]\n[features]\nhost=['dep:fixture-platform']\n[{header}]\nfixture-platform={{path='platform',optional=true}}\n")).unwrap();
        failed(
            &project.run(&["check", "--structure-only"]),
            &["fixture-platform", "normal", "host package"],
        );
    }
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn portable_platform_edge_reports_target_kind_and_remedy() {
    let project = Project::new(Some("application"));
    project.dependency("fixture-platform", "platform", "#![no_std]\n");
    fs::write(project.root.join("Cargo.toml"), format!("{MANIFEST}[package.metadata.snap]\nrole='application'\n[features]\nbrowser=['dep:fixture-platform']\n[target.'cfg(target_arch=\"wasm32\")'.dependencies]\nfixture-platform={{path='fixture-platform',optional=true}}\n")).unwrap();
    failed(
        &project.run(&["check", "--structure-only"]),
        &[
            "cli-fixture",
            "fixture-platform",
            "normal",
            "wasm32",
            "host package",
        ],
    );
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn new_workspace_portable_package_is_checked_automatically() {
    let project = Project::new(Some("application"));
    project.dependency(
        "extra",
        "core",
        "pub fn host_only() { std::thread::yield_now(); }\n",
    );
    fs::write(
        project.root.join("Cargo.toml"),
        format!(
            "{}[package.metadata.snap]\nrole='application'\n",
            MANIFEST.replace("[workspace]", "[workspace]\nmembers=['extra']")
        ),
    )
    .unwrap();
    passed(&project.run(&["check", "--structure-only"]));
    failed(
        &project.run(&["check", "--structure-only", "--workspace"]),
        &["extra", "wasm32v1-none"],
    );
}

#[test]
#[ignore = "compiler/filesystem contract"]
fn missing_roles_and_shared_dev_application_edges_are_errors() {
    let project = Project::new(Some("core"));
    project.dependency("fixture-app", "application", "#![no_std]\n");
    fs::write(project.root.join("Cargo.toml"), format!("{MANIFEST}[package.metadata.snap]\nrole='core'\n[dev-dependencies]\nfixture-app={{path='fixture-app'}}\n")).unwrap();
    failed(
        &project.run(&["check", "--structure-only"]),
        &["dev", "fixture-app"],
    );
    fs::write(project.root.join("Cargo.toml"), MANIFEST).unwrap();
    failed(
        &project.run(&["check", "--structure-only"]),
        &["package.metadata.snap.role"],
    );
}
