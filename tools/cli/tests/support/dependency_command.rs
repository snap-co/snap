//! An external tool recorder for the dependency coordinator's CLI contract.
use std::{env, fs, io::Write};

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    match args[0].as_str() {
        "metadata" => print!("{}", fs::read_to_string("metadata.json").unwrap()),
        tool if args.get(1).map(String::as_str) == Some("--version") => {
            println!(
                "cargo-{tool} {}",
                fs::read_to_string(format!("{tool}.version")).unwrap()
            );
        }
        _ => {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("invocations")
                .unwrap();
            writeln!(file, "{}", args.join("\n")).unwrap();
            writeln!(file, "END").unwrap();
        }
    }
}
