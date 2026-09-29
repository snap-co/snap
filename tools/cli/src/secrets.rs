use crate::config::Project;
use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{io::Write, path::Path};

#[derive(Parser)]
pub struct Args {
    #[command(subcommand)]
    command: Operation,
}
#[derive(Subcommand)]
enum Operation {
    /// Explicitly create a private age identity, public recipient and empty bag.
    Init {
        #[arg(default_value = "development")]
        environment: String,
    },
    /// Encrypt the private secrets.toml using recipients.txt, without a private key.
    Seal {
        #[arg(default_value = "development")]
        environment: String,
    },
}
pub fn run(args: Args) -> Result<()> {
    let project = Project::discover(None)?;
    let (environment, init) = match args.command {
        Operation::Init { environment } => (environment, true),
        Operation::Seal { environment } => (environment, false),
    };
    ensure!(
        !environment.is_empty()
            && environment
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "Invalid environment name"
    );
    let directory = project.root.join(".deployment").join(environment);
    ensure!(
        directory.join("config.toml").is_file(),
        "Create .deployment/<environment>/config.toml first"
    );
    if init {
        for name in [
            "secrets.key",
            "recipients.txt",
            "secrets.toml",
            "secrets.enc",
        ] {
            ensure!(
                !directory.join(name).try_exists()?,
                "Secret setup already exists; refusing to overwrite"
            );
        }
        let identity = age::x25519::Identity::generate();
        create(
            &directory.join("secrets.key"),
            identity.to_string().expose_secret().as_bytes(),
            true,
        )?;
        create(
            &directory.join("recipients.txt"),
            identity.to_public().to_string().as_bytes(),
            false,
        )?;
        create(
            &directory.join("secrets.toml"),
            b"# Private authoring file. Run snap secrets seal after editing.\n",
            true,
        )?;
    }
    let text = std::fs::read_to_string(directory.join("recipients.txt"))
        .context("Cannot read recipients.txt")?;
    let recipients: Vec<age::x25519::Recipient> = text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
        .map(|s| {
            s.parse()
                .map_err(|_| anyhow::anyhow!("Invalid age recipient"))
        })
        .collect::<Result<_>>()?;
    let plaintext =
        std::fs::read(directory.join("secrets.toml")).context("Cannot read secrets.toml")?;
    let encrypted = snap_config::Secrets::encrypt(&plaintext, &recipients)?;
    let temporary = tempfile::NamedTempFile::new_in(&directory)?;
    std::fs::write(temporary.path(), encrypted)?;
    temporary.persist(directory.join("secrets.enc"))?;
    println!("Encrypted secrets written. Private identity and plaintext are never packaged.");
    Ok(())
}
fn create(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(if private { 0o600 } else { 0o644 });
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}
