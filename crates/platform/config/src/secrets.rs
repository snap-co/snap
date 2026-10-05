//! One symmetric key seals and opens a deployment's secrets bag. No recipients.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox, SecretString, zeroize::Zeroizing};
use serde::Deserialize;
use std::{collections::BTreeMap, io::Read, path::Path, str::FromStr};

const KEY_PREFIX: &str = "SNAP-SECRET-KEY-1:";
// Format v1: authenticated version header, 24-byte random nonce, ciphertext
// with its 16-byte Poly1305 tag. New nonces are required for every seal, even
// when plaintext is unchanged. This format is deliberately not Age-compatible.
const HEADER: &[u8; 8] = b"SNAPSEC\x01";
const NONCE_LEN: usize = 24;

#[derive(Debug)]
pub struct MasterKey(SecretBox<[u8; 32]>);
impl MasterKey {
    /// Generate a random encryption key, never derive one from a password.
    pub fn generate() -> Result<Self> {
        let mut key = Self(SecretBox::default());
        getrandom::fill(key.0.expose_secret_mut())
            .map_err(|_| anyhow::anyhow!("Cannot generate secrets key"))?;
        Ok(key)
    }

    /// Explicit export for secrets.key or SNAP_MASTER_KEY. Never log this value.
    pub fn encode(&self) -> Secret {
        format!(
            "{KEY_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(self.0.expose_secret())
        )
        .into()
    }

    pub fn read(path: &Path) -> Result<Self> {
        let mut file = std::fs::File::open(path).context("Cannot open secrets.key")?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= 4096,
            "Invalid secrets.key file"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                metadata.permissions().mode() & 0o077 == 0,
                "secrets.key must be private; use chmod 600"
            );
        }
        let mut value = Zeroizing::new(String::new());
        file.read_to_string(&mut value)
            .context("Cannot read secrets.key")?;
        value.trim().parse()
    }
}
impl FromStr for MasterKey {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        ensure!(
            !value.starts_with("AGE-SECRET-KEY-"),
            "Legacy Age key: preserve the old key and bag, then reseal secrets.toml with a new symmetric key"
        );
        let encoded = value
            .strip_prefix(KEY_PREFIX)
            .context("Invalid secrets key format")?;
        let mut key = Self(SecretBox::default());
        let length = URL_SAFE_NO_PAD
            .decode_slice(encoded, key.0.expose_secret_mut())
            .map_err(|_| anyhow::anyhow!("Invalid secrets key encoding"))?;
        ensure!(length == 32, "Secrets key must contain exactly 32 bytes");
        Ok(key)
    }
}

#[derive(Clone)]
pub struct SecretRef(String);
impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        if name.split('.').any(str::is_empty)
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        {
            return Err(serde::de::Error::custom("Invalid secret reference"));
        }
        Ok(Self(name))
    }
}
impl SecretRef {
    pub fn name(&self) -> &str {
        &self.0
    }
}
pub struct Secret(SecretString);
impl Clone for Secret {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value.into())
    }
}
impl Secret {
    pub fn expose(&self) -> &str {
        self.0.expose_secret()
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

#[derive(Default)]
pub struct Secrets(BTreeMap<String, Secret>);
impl Secrets {
    pub fn resolve(&self, reference: &SecretRef) -> Result<&Secret> {
        self.0
            .get(reference.name())
            .with_context(|| format!("Missing secret: {}", reference.name()))
    }

    pub fn encrypt(plaintext: &[u8], key: &MasterKey) -> Result<Vec<u8>> {
        Self::parse(plaintext)?;
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("Cannot generate secrets nonce"))?;
        let cipher = XChaCha20Poly1305::new(key.0.expose_secret().into());
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: HEADER,
                },
            )
            .map_err(|_| anyhow::anyhow!("Secrets encryption failed"))?;
        Ok([HEADER.as_slice(), nonce.as_slice(), ciphertext.as_slice()].concat())
    }

    /// Recover the original TOML, including comments, for private authoring.
    /// Authentication and bag validation complete before any plaintext is returned.
    pub fn unseal(ciphertext: &[u8], key: &MasterKey) -> Result<Secret> {
        let plaintext = Self::plaintext(ciphertext, key)?;
        Self::parse(&plaintext)?;
        Ok(std::str::from_utf8(&plaintext)
            .map_err(|_| anyhow::anyhow!("Invalid secrets bag"))?
            .to_owned()
            .into())
    }

    pub fn decrypt(ciphertext: &[u8], key: &MasterKey) -> Result<Self> {
        Self::parse(&Self::plaintext(ciphertext, key)?)
    }

    fn plaintext(ciphertext: &[u8], key: &MasterKey) -> Result<Zeroizing<Vec<u8>>> {
        ensure!(
            !ciphertext.starts_with(b"age-encryption.org/"),
            "Legacy Age bag: preserve it and decrypt with Age before resealing with a new symmetric key"
        );
        ensure!(
            ciphertext.len() >= HEADER.len() + NONCE_LEN + 16 && ciphertext.starts_with(HEADER),
            "Invalid or unsupported secrets bag format"
        );
        let (nonce, ciphertext) = ciphertext[HEADER.len()..].split_at(NONCE_LEN);
        let cipher = XChaCha20Poly1305::new(key.0.expose_secret().into());
        Ok(Zeroizing::new(
            cipher
                .decrypt(
                    XNonce::from_slice(nonce),
                    Payload {
                        msg: ciphertext,
                        aad: HEADER,
                    },
                )
                .map_err(|_| anyhow::anyhow!("Secrets decryption failed"))?,
        ))
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        let text =
            std::str::from_utf8(bytes).map_err(|_| anyhow::anyhow!("Invalid secrets bag"))?;
        let table: toml::Table =
            toml::from_str(text).map_err(|_| anyhow::anyhow!("Invalid secrets bag"))?;
        fn collect(
            prefix: &str,
            table: toml::Table,
            output: &mut BTreeMap<String, Secret>,
        ) -> Result<()> {
            for (key, value) in table {
                ensure!(
                    !key.is_empty() && !key.contains('.'),
                    "Secret keys must be nonempty without dots"
                );
                let name = if prefix.is_empty() {
                    key
                } else {
                    format!("{prefix}.{key}")
                };
                match value {
                    toml::Value::String(s) => {
                        output.insert(name, Secret(s.into()));
                    }
                    toml::Value::Table(t) => collect(&name, t, output)?,
                    _ => bail!("Secrets must be strings or nested tables"),
                }
            }
            Ok(())
        }
        let mut output = BTreeMap::new();
        collect("", table, &mut output)?;
        Ok(Self(output))
    }
}
