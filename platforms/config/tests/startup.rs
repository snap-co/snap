use serde::Deserialize;
use snap_config::{Config, SecretRef, Secrets};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct App {
    client_secret_ref: SecretRef,
}

#[test]
fn startup_resolves_an_age_bag_without_exposing_it_in_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let identity = age::x25519::Identity::generate();
    let bag = Secrets::encrypt(
        b"[oauth]\nclient_secret = 'private-test-credential'\n",
        &[identity.to_public()],
    )
    .unwrap();
    std::fs::write(directory.path().join("secrets.enc"), &bag).unwrap();
    let path = directory.path().join("config.toml");
    std::fs::write(&path, "version = 1\n[host]\nmode = 'development'\nlisten = '127.0.0.1:0'\ndata_dir = 'data'\n[app]\nclient_secret_ref = 'oauth.client_secret'\n").unwrap();
    let config = Config::<App>::read(&path).unwrap();
    assert_eq!(
        config.database(),
        directory.path().join("data/store.sqlite")
    );
    let secrets = config.secrets(Some(&identity)).unwrap();
    let secret = secrets.resolve(&config.app.client_secret_ref).unwrap();
    assert_eq!(secret.expose(), "private-test-credential");
    assert!(!format!("{secret:?}").contains("private-test-credential"));
    assert!(config.secrets(None).is_err());
    let wrong_key = age::x25519::Identity::generate();
    let error = config.secrets(Some(&wrong_key)).err().unwrap().to_string();
    assert!(!error.contains("private-test-credential"));
    let mut corrupted = bag;
    let last = corrupted.len() - 1;
    corrupted[last] ^= 1;
    std::fs::write(directory.path().join("secrets.enc"), corrupted).unwrap();
    assert!(config.secrets(Some(&identity)).is_err());
}

#[test]
fn configuration_rejects_unknown_fields_and_unsafe_production_modes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    for text in [
        "version=2\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n[app]\nclient_secret_ref='a'\n",
        "version=1\n[host]\nmode='development'\nlisten='0.0.0.0:80'\ndata_dir='data'\n[app]\nclient_secret_ref='a'\n",
        "version=1\n[host]\nmode='production'\nlisten='0.0.0.0:80'\norigin='http://example.com'\ndata_dir='/data'\n[app]\nclient_secret_ref='a'\n",
        "version=1\n[host]\nmode='production'\nlisten='0.0.0.0:80'\norigin='https://example.com'\ndata_dir='/data'\ndev_origins=['http://localhost']\n[app]\nclient_secret_ref='a'\n",
        "version=1\n[host]\nmode='development'\nlisten='127.0.0.1:0'\ndata_dir='data'\n[app]\nclient_secret_ref='a'\nunrecognized='private-value'\n",
    ] {
        std::fs::write(&path, text).unwrap();
        let error = Config::<App>::read(&path)
            .err()
            .expect("invalid config rejected")
            .to_string();
        assert!(!error.contains("private-value"));
    }
    std::fs::write(&path, "version=1\n[host]\nmode='production'\nlisten='0.0.0.0:80'\norigin='https://example.com'\ndata_dir='/data'\n[app]\nclient_secret_ref='a'\n").unwrap();
    assert!(Config::<App>::read(&path).is_ok());
}
