use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rsa::{
    RsaPrivateKey,
    signature::{SignatureEncoding, Signer},
    traits::PublicKeyParts,
};
use serde_json::{Value, json};
use sha2::Sha256;
use snap_identity::Crypto;

fn native() -> snap_crypto::Native {
    snap_crypto::Native
}

#[test]
#[ignore = "native crypto"]
fn jwks_verification_rejects_tampering_and_key_confusion() {
    let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
    let public = json!({"kty":"RSA","kid":"fixture","alg":"RS256","use":"sig","n":URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),"e":URL_SAFE_NO_PAD.encode(key.e().to_bytes_be())});
    let jwks = json!({"keys":[public]});
    let sign = |header: Value| {
        let input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(json!({"sub":"person"}).to_string())
        );
        let signature =
            rsa::pkcs1v15::SigningKey::<Sha256>::new(key.clone()).sign(input.as_bytes());
        format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()))
    };
    let token = sign(json!({"alg":"RS256","kid":"fixture"}));
    assert_eq!(
        native().verify_token(&token, &jwks).unwrap()["sub"],
        "person"
    );
    for header in [
        json!({"alg":"none","kid":"fixture"}),
        json!({"alg":"HS256","kid":"fixture"}),
        json!({"alg":"RS256","kid":"other"}),
        json!({"alg":"RS256","kid":"fixture","crit":["unknown"]}),
    ] {
        assert!(native().verify_token(&sign(header), &jwks).is_err());
    }
    let mut parts: Vec<_> = token.split('.').map(str::to_string).collect();
    parts[1] = URL_SAFE_NO_PAD.encode(json!({"sub":"attacker"}).to_string());
    assert!(native().verify_token(&parts.join("."), &jwks).is_err());
    assert!(
        native()
            .verify_token(&token, &json!({"keys":[jwks["keys"][0],jwks["keys"][0]]}))
            .is_err()
    );
    let mut signing_only = jwks.clone();
    signing_only["keys"][0]["key_ops"] = json!(["sign"]);
    assert!(native().verify_token(&token, &signing_only).is_err());
}

/// A token's own header never redirects key selection. The JWKS a caller passes
/// is the only source of candidate keys, so a foreign issuer's JWKS cannot
/// satisfy a pinned issuer's token.
#[test]
#[ignore = "native crypto"]
fn verification_ignores_token_supplied_key_locations() {
    let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 2048).unwrap();
    let jwks = json!({"keys":[{
        "kty":"RSA","kid":"fixture","alg":"RS256","use":"sig",
        "n":URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),
        "e":URL_SAFE_NO_PAD.encode(key.e().to_bytes_be()),
    }]});
    let claims = json!({"sub":"person"});
    let header = json!({"alg":"RS256","kid":"fixture","jku":"https://attacker.invalid/jwks","x5u":"https://attacker.invalid/x5u"});
    let input = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(header.to_string()),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let signature = rsa::pkcs1v15::SigningKey::<Sha256>::new(key).sign(input.as_bytes());
    let token = format!(
        "{input}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    );
    assert_eq!(native().verify_token(&token, &jwks).unwrap()["sub"], "person");
    // An empty or unrelated JWKS supplies no candidate key for the header's kid.
    assert!(native().verify_token(&token, &json!({"keys":[]})).is_err());
}

/// Hosts that cannot verify RS256 must refuse the token rather than accept it.
#[test]
fn unsupported_crypto_host_refuses_tokens() {
    struct Blind;
    impl Crypto for Blind {
        fn random(&mut self) -> Result<[u8; 32], snap_store::Error> {
            Ok([0; 32])
        }
        fn hash_password(&mut self, _: &str) -> Result<String, snap_store::Error> {
            Err(snap_store::Error::Unavailable)
        }
        fn verify_password(&self, _: &str, _: &str) -> Result<bool, snap_store::Error> {
            Ok(false)
        }
        fn digest(&self, _: &str) -> Vec<u8> {
            Vec::new()
        }
    }
    assert!(matches!(
        Blind.verify_token("a.b.c", &json!({"keys":[]})),
        Err(snap_store::Error::Unavailable)
    ));
}