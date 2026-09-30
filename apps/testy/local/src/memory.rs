fn main() {
    let server = snap_transport::server::Server::new(testy::TestAuthority, Default::default());
    let execution = snap_transport::execution::Executor::new(testy::App::default(), 1024).unwrap();
    let platform = snap_platform_local::memory::Memory::new(
        snap_transport::execution::Runtime::new(server, execution),
    );
    let mut client = testy::Client::new(platform.channel());
    client.use_session(testy::BEARER).unwrap(); // Explicit fixture authority; no credential issuance.
    let result =
        futures::executor::block_on(testy::journey(&mut client, "memory-program")).unwrap();
    assert_eq!(result.accumulator, 6);
    assert_eq!(result.history.len(), 4);
    println!(
        "Testy memory: {} after {} operations",
        result.accumulator,
        result.history.len()
    );
}
