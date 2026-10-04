//! A tooling-owned application input for the real packaging and dev commands.
//! Its host loads production deployment config; it supplies no CLI outcomes.
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn project() -> tempfile::TempDir {
    let cache = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/coding-agents");
    fs::create_dir_all(&cache).unwrap();
    let project = tempfile::Builder::new()
        .prefix("snap-package-contract-")
        .tempdir_in(cache)
        .unwrap();
    let root = project.path();
    fs::create_dir_all(root.join("src")).unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    fs::write(root.join("Cargo.toml"), "[package]\nname='fixture'\nversion='0.0.0'\nedition='2024'\n[workspace]\nmembers=['server','web/wasm']\n[package.metadata.snap]\nrole='application'\n").unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "#![no_std]\npub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("server/src")).unwrap();
    fs::write(root.join("server/Cargo.toml"), format!("[package]\nname='fixture-server'\nversion='0.0.0'\nedition='2024'\n[dependencies]\nfixture={{path='..'}}\nsnap-config={{path={:?}}}\nserde={{version='1',features=['derive']}}\n[package.metadata.snap]\nrole='host'\n", repository.join("crates/platform/config"))).unwrap();
    fs::write(
        root.join("server/src/main.rs"),
        include_str!("../fixtures/package-host.rs"),
    )
    .unwrap();
    fs::create_dir_all(root.join("web/wasm/src")).unwrap();
    fs::write(root.join("web/wasm/Cargo.toml"), "[package]\nname='fixture-wasm'\nversion='0.0.0'\nedition='2024'\n[lib]\ncrate-type=['cdylib']\n[dependencies]\nfixture={path='../..'}\nwasm-bindgen='=0.2.128'\n[package.metadata.snap]\nrole='platform'\n").unwrap();
    fs::write(root.join("web/wasm/src/lib.rs"), "use wasm_bindgen::prelude::*;\n#[wasm_bindgen]\npub fn answer() -> u32 { fixture::answer() }\n").unwrap();
    fs::write(root.join("web/app.tsx"), "import init, { answer } from '@snap/wasm';\nawait init();\ndocument.getElementById('root')!.textContent = String(answer());\n").unwrap();
    fs::write(
        root.join("web/client.ts"),
        "export { default, answer } from '@snap/wasm';\n",
    )
    .unwrap();
    fs::write(
        root.join("web/index.html"),
        "<!doctype html><div id='root'></div><script type='module' src='./app.js'></script>\n",
    )
    .unwrap();
    fs::write(root.join("snap.toml"), "version=1\napplication='fixture'\n[build.clients.web]\nkind='web'\nsource='web'\nwasm='web/wasm/Cargo.toml'\nsdk='web/client.ts'\n[build.servers.native]\nmanifest='server/Cargo.toml'\nbinary='fixture-server'\ntargets=['host']\n").unwrap();
    let lock = std::process::Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "{}",
        String::from_utf8_lossy(&lock.stderr)
    );
    project
}
