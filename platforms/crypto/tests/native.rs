use snap_identity::Crypto;

#[test]
#[ignore = "real password hashing integration gate"]
fn native_passwords_and_tokens_use_real_crypto() {
    let mut crypto = snap_crypto::Native;
    let first = crypto.hash_password("password1").unwrap();
    let second = crypto.hash_password("password1").unwrap();
    assert!(first.starts_with("$argon2id$"));
    assert_ne!(first, second);
    assert!(crypto.verify_password("password1", &first).unwrap());
    assert!(!crypto.verify_password("password2", &first).unwrap());
    assert!(crypto.verify_password("password1", "corrupt").is_err());
    assert_ne!(crypto.random().unwrap(), crypto.random().unwrap());
    assert_eq!(
        crypto.digest("abc"),
        [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad
        ]
    );
}
