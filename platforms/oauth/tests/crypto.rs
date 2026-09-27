use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rsa::{
    RsaPrivateKey,
    signature::{SignatureEncoding, Signer},
    traits::PublicKeyParts,
};
use serde_json::{Value, json};
use sha2::Sha256;

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
        snap_oauth_local::verify(&token, &jwks).unwrap()["sub"],
        "person"
    );
    for header in [
        json!({"alg":"none","kid":"fixture"}),
        json!({"alg":"HS256","kid":"fixture"}),
        json!({"alg":"RS256","kid":"other"}),
        json!({"alg":"RS256","kid":"fixture","crit":["unknown"]}),
    ] {
        assert!(snap_oauth_local::verify(&sign(header), &jwks).is_err());
    }
    let mut parts: Vec<_> = token.split('.').map(str::to_string).collect();
    parts[1] = URL_SAFE_NO_PAD.encode(json!({"sub":"attacker"}).to_string());
    assert!(snap_oauth_local::verify(&parts.join("."), &jwks).is_err());
    assert!(
        snap_oauth_local::verify(&token, &json!({"keys":[jwks["keys"][0],jwks["keys"][0]]}))
            .is_err()
    );
    let mut signing_only = jwks.clone();
    signing_only["keys"][0]["key_ops"] = json!(["sign"]);
    assert!(snap_oauth_local::verify(&token, &signing_only).is_err());
}
