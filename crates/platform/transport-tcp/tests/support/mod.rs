//! Disposable test PKI shared by real-carrier consumers. Never linked into hosts.
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use snap_transport_tcp::tls::{ClientTls, ServerTls};
use std::path::Path;

pub fn pki(directory: &Path, expired: bool) -> (ServerTls, ClientTls) {
    let mut ca = CertificateParams::new(vec![]).unwrap();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca_key = KeyPair::generate().unwrap();
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = Issuer::new(ca, ca_key);
    let mut leaf =
        CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into(), "::1".into()]).unwrap();
    leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    if expired {
        leaf.not_before = rcgen::date_time_ymd(2000, 1, 1);
        leaf.not_after = rcgen::date_time_ymd(2001, 1, 1);
    }
    let leaf_key = KeyPair::generate().unwrap();
    let cert = leaf.signed_by(&leaf_key, &issuer).unwrap();
    let ca_file = directory.join("ca.pem");
    let cert_file = directory.join("server.pem");
    let key_file = directory.join("server-key.pem");
    std::fs::write(&ca_file, ca_cert.pem()).unwrap();
    std::fs::write(&cert_file, cert.pem()).unwrap();
    std::fs::write(&key_file, leaf_key.serialize_pem()).unwrap();
    (
        ServerTls::new(&cert_file, &key_file).unwrap(),
        ClientTls::new(Some(&ca_file), None).unwrap(),
    )
}
