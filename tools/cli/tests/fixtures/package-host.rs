use serde::Deserialize;
use std::{
    io::{Read, Write},
    net::TcpListener,
};

#[derive(Deserialize)]
struct Application {
    oauth: OAuth,
}
#[derive(Deserialize)]
struct OAuth {
    client_secret_ref: snap_config::SecretRef,
}

fn main() {
    let config = snap_config::Config::<Application>::read(
        &snap_config::application_config("fixture").unwrap(),
    )
    .unwrap();
    if std::env::args().any(|arg| arg == "--migrate") {
        std::fs::create_dir_all(config.path(&config.host.data_dir)).unwrap();
        return;
    }
    let secrets = config.load_secrets().unwrap();
    assert!(
        !secrets
            .resolve(&config.app.oauth.client_secret_ref)
            .unwrap()
            .expose()
            .is_empty()
    );
    let listener = TcpListener::bind(config.host.listen).unwrap();
    println!("Fixture http://{}", listener.local_addr().unwrap());
    std::io::stdout().flush().unwrap();
    for connection in listener.incoming() {
        let mut connection = connection.unwrap();
        connection
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = [0; 4096];
        connection.read(&mut request).unwrap();
        let body = fixture::answer().to_string();
        write!(
            connection,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }
}
