use snap_config::{Artifacts, NativeArtifact, packaged_config};
use std::{collections::BTreeMap, fs, path::PathBuf};

#[test]
fn nested_target_binaries_find_relocated_config_without_changing_resource_paths() {
    let temporary = tempfile::tempdir().unwrap();
    let package = temporary.path().join("package");
    let executable = PathBuf::from("servers/native/x86_64-unknown-linux-gnu/server");
    fs::create_dir_all(package.join(executable.parent().unwrap())).unwrap();
    let inventory = Artifacts {
        version: 1,
        application: "chatty".into(),
        clients: BTreeMap::from([("web".into(), PathBuf::from("clients/web"))]),
        native_clients: vec![],
        servers: vec![NativeArtifact {
            name: "native".into(),
            target: "x86_64-unknown-linux-gnu".into(),
            executable: executable.clone(),
            args: vec![],
        }],
    };
    fs::write(
        package.join("artifacts.toml"),
        toml::to_string(&inventory).unwrap(),
    )
    .unwrap();
    fs::write(package.join("config.toml"), "fixture configuration").unwrap();
    let relocated = temporary.path().join("relocated");
    fs::rename(package, &relocated).unwrap();
    let directory = relocated.join(executable.parent().unwrap());
    assert_eq!(
        packaged_config(&directory, Some("chatty")).unwrap(),
        Some(relocated.join("config.toml"))
    );
    assert_eq!(
        packaged_config(&directory, None).unwrap(),
        Some(relocated.join("config.toml"))
    );
    assert_eq!(packaged_config(&directory, Some("authy")).unwrap(), None);
    fs::remove_file(relocated.join("config.toml")).unwrap();
    assert!(packaged_config(&directory, Some("chatty")).is_err());
    fs::write(relocated.join("artifacts.toml"), "invalid inventory").unwrap();
    assert!(packaged_config(&directory, Some("chatty")).is_err());
}
