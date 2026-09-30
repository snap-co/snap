//! Subprocess behavior hosted by the integration-test executable, never production.
use std::{env, fs, io::Write, net::TcpListener, process::Command, time::Duration};

pub fn run() {
    let mut argv = env::args().skip(1);
    let mut args = Vec::new();
    while let Some(arg) = argv.next() {
        if arg == "--skip" {
            args.push(argv.next().unwrap());
        }
    }
    if args.is_empty() {
        return;
    }
    match args[0].as_str() {
        "exit" => std::process::exit(args[1].parse().unwrap()),
        "print" => println!("{}", args[1]),
        "record" => {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("ran")
                .unwrap();
            writeln!(
                file,
                "{}:{}",
                args[1],
                env::var("SNAP_TEST_PLATFORM").unwrap_or_else(|_| "check".into())
            )
            .unwrap();
        }
        "literal" => {
            for key in [
                "SNAP_CHECK_EXECUTABLE",
                "SNAP_CHECK_PACKAGE",
                "SNAP_CHECK_WEB_DIR",
            ] {
                assert!(env::var_os(key).is_none(), "Inherited {key}");
            }
            fs::write(
                "cwd",
                env::current_dir().unwrap().as_os_str().as_encoded_bytes(),
            )
            .unwrap();
            let mut file = fs::File::create("argv").unwrap();
            for arg in &args[1..] {
                file.write_all(arg.as_bytes()).unwrap();
                file.write_all(&[0]).unwrap();
            }
        }
        "listen" => {
            // A descendant that ignores graceful shutdown must still be retired.
            use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
            let mut blocked = SigSet::empty();
            blocked.add(Signal::SIGTERM);
            pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), None).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            fs::write(
                "ready.pending",
                format!(
                    "{} {}",
                    listener.local_addr().unwrap().port(),
                    std::process::id()
                ),
            )
            .unwrap();
            fs::rename("ready.pending", "ready").unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "descendant" => {
            let mut child = Command::new(env::current_exe().unwrap())
                .args([
                    "--exact",
                    "fixture_child",
                    "--ignored",
                    "--nocapture",
                    "--skip",
                    "listen",
                ])
                .spawn()
                .unwrap();
            child.wait().unwrap();
        }
        _ => panic!("Unknown fixture command"),
    }
}
