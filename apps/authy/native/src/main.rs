fn main() -> std::io::Result<()> {
    authy_native::run(snap_store::NoCache)
}
