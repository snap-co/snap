use crate::config::Project;
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
    /// Create one secrets.key and an empty secrets.enc, refusing existing files.
    Init {
        #[arg(default_value = "development")]
        environment: String,
    },
    /// Encrypt secrets.toml into secrets.enc using secrets.key.
    Seal {
        #[arg(default_value = "development")]
        environment: String,
    },
    /// Decrypt secrets.enc into private secrets.toml, refusing to overwrite it.
    Unseal {
        #[arg(default_value = "development")]
        environment: String,
    },
}
pub fn run(args: Args) -> Result<()> {
    let project = Project::discover(None)?;
    let (environment, init, unseal) = match args.command {
        Operation::Init { environment } => (environment, true, false),
        Operation::Seal { environment } => (environment, false, false),
        Operation::Unseal { environment } => (environment, false, true),
    };
    ensure!(
        !environment.is_empty()
            && environment
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "Invalid environment name"
    );
    let directory = if environment == "development" {
        snap_config::development_config(&project.root)?
            .parent()
            .context("Development config directory missing")?
            .to_owned()
    } else {
        project.root.join(".deployment").join(environment)
    };
    ensure!(
        directory.join("config.toml").is_file(),
        "Create the development profile or .deployment/<environment>/config.toml first"
    );
    if init {
        // A leftover legacy recipients file also means a setup already exists.
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
        let key = snap_config::MasterKey::generate()?;
        create(
            &directory.join("secrets.key"),
            key.encode().expose().as_bytes(),
        )?;
        create(
            &directory.join("secrets.toml"),
            b"# Private authoring file. Run snap secrets seal after editing.\n",
        )?;
    }
    let key = snap_config::MasterKey::read(&directory.join("secrets.key"))?;
    if unseal {
        let ciphertext =
            std::fs::read(directory.join("secrets.enc")).context("Cannot read secrets.enc")?;
        let plaintext = snap_config::Secrets::unseal(&ciphertext, &key)?;
        create(
            &directory.join("secrets.toml"),
            plaintext.expose().as_bytes(),
        )?;
        println!("Decrypted secrets written to private secrets.toml.");
        return Ok(());
    }
    let plaintext = snap_config::Secret::from(
        std::fs::read_to_string(directory.join("secrets.toml"))
            .context("Cannot read secrets.toml")?,
    );
    let encrypted = snap_config::Secrets::encrypt(plaintext.expose().as_bytes(), &key)?;
    let temporary = tempfile::NamedTempFile::new_in(&directory)?;
    std::fs::write(temporary.path(), encrypted)?;
    temporary.persist(directory.join("secrets.enc"))?;
    println!("Encrypted secrets written. secrets.key and secrets.toml are never packaged.");
    Ok(())
}
fn create(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .context("Cannot create private secrets file; existing files are never overwritten")?
        .write_all(bytes)?;
    Ok(())
}
