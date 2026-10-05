use serde::Deserialize;
use std::{
    io::{BufRead, BufReader, Write},
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
    let options = snap_config::Options::parse().unwrap();
    let config = snap_config::Config::<Application>::read(&options.config).unwrap();
    match options.action {
        snap_config::Action::Check => return,
        snap_config::Action::Migrate => {
            std::fs::create_dir_all(config.path(&config.host.data_dir)).unwrap();
            return;
        }
        snap_config::Action::Serve => {}
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
        // Consume the whole GET header before closing. A partial socket read can
        // leave unread bytes and turn an otherwise valid response into a reset.
        {
            let mut request = BufReader::new(&mut connection);
            let mut line = String::new();
            loop {
                line.clear();
                if request.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
        }
        let body = fixture::answer().to_string();
        write!(
            connection,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }
}
