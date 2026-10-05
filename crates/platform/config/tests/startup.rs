use serde::Deserialize;
use snap_config::{Config, MasterKey, SecretRef, Secrets};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct App {
    client_secret_ref: SecretRef,
}

#[test]
fn startup_resolves_a_symmetric_bag_without_exposing_it_in_diagnostics() {
    let directory = tempfile::tempdir().unwrap();
    let key = MasterKey::generate().unwrap();
    let bag = Secrets::encrypt(
        b"[oauth]\nclient_secret = 'private-test-credential'\n",
        &key,
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
    let secrets = config.secrets(Some(&key)).unwrap();
    let secret = secrets.resolve(&config.app.client_secret_ref).unwrap();
    assert_eq!(secret.expose(), "private-test-credential");
    assert!(!format!("{secret:?}").contains("private-test-credential"));
    assert!(config.secrets(None).is_err());
    let wrong_key = MasterKey::generate().unwrap();
    let error = config.secrets(Some(&wrong_key)).err().unwrap().to_string();
    assert!(!error.contains("private-test-credential"));
    // Header, nonce, ciphertext and tag are all protected; truncation must
    // reject rather than panic or return partially authenticated plaintext.
    for index in [0, 7, 8, 32, bag.len() - 1] {
        let mut corrupted = bag.clone();
        corrupted[index] ^= 1;
        std::fs::write(directory.path().join("secrets.enc"), corrupted).unwrap();
        assert!(config.secrets(Some(&key)).is_err());
    }
    for length in 0..bag.len() {
        std::fs::write(directory.path().join("secrets.enc"), &bag[..length]).unwrap();
        assert!(config.secrets(Some(&key)).is_err());
    }
    let plaintext = b"[oauth]\nclient_secret = 'private-test-credential'\n";
    let resealed = Secrets::encrypt(plaintext, &key).unwrap();
    assert_ne!(bag, resealed, "Repeated seals must use fresh nonces");
    assert_eq!(
        Secrets::unseal(&resealed, &key)
            .unwrap()
            .expose()
            .as_bytes(),
        plaintext
    );
    assert!(!format!("{key:?}").contains(key.encode().expose()));
    assert!(Secrets::encrypt(b"not TOML: private-test-credential", &key).is_err());
}

#[test]
fn master_keys_round_trip_and_reject_passwords_and_legacy_formats() {
    let key = MasterKey::generate().unwrap();
    let encoded = key.encode();
    assert_eq!(
        encoded
            .expose()
            .parse::<MasterKey>()
            .unwrap()
            .encode()
            .expose(),
        encoded.expose()
    );
    for invalid in [
        "password",
        "SNAP-SECRET-KEY-1:",
        "SNAP-SECRET-KEY-1:AA",
        "SNAP-SECRET-KEY-2:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        "SNAP-SECRET-KEY-1:private-invalid-base64-credential!",
    ] {
        let error = invalid.parse::<MasterKey>().unwrap_err().to_string();
        assert!(!error.contains("private-invalid-base64-credential"));
    }
    assert!(
        "AGE-SECRET-KEY-1PRIVATE"
            .parse::<MasterKey>()
            .unwrap_err()
            .to_string()
            .contains("Legacy Age key")
    );
    assert!(
        Secrets::unseal(b"age-encryption.org/v1\n", &key)
            .err()
            .unwrap()
            .to_string()
            .contains("Legacy Age bag")
    );
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
