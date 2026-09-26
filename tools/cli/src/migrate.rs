use anyhow::{Context, Result, ensure};
use clap::{Args as ClapArgs, Subcommand};
use snap_store::resident::{identifier, migration::Migration};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(ClapArgs)]
pub struct Args {
    #[command(subcommand)]
    action: Option<Action>,
    /// SQLite database file. Required when applying or inspecting migrations.
    #[arg(long)]
    database: Option<PathBuf>,
    /// Directory of ordered, immutable TOML migration declarations
    #[arg(long, default_value = "migrations", global = true)]
    migrations: PathBuf,
    /// Validate history and list applied/pending migrations without applying them
    #[arg(long)]
    status: bool,
}

#[derive(Subcommand)]
enum Action {
    /// Create an editable migration template; fill its changes before applying
    New { name: String },
}

fn read(directory: &Path) -> Result<Vec<Migration>> {
    let mut paths = std::fs::read_dir(directory)
        .with_context(|| format!("read {}", directory.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|p| p.extension().is_some_and(|e| e == "toml"));
    paths.sort();
    let mut migrations = Vec::new();
    for path in paths {
        let migration: Migration = toml::from_str(&std::fs::read_to_string(&path)?)
            .with_context(|| format!("parse {}", path.display()))?;
        ensure!(
            path.file_stem().and_then(|s| s.to_str()) == Some(&migration.id),
            "migration ID must match filename: {}",
            path.display()
        );
        migrations.push(migration);
    }
    Ok(migrations)
}

pub fn run(args: Args) -> Result<()> {
    if let Some(Action::New { name }) = args.action {
        ensure!(
            args.database.is_none() && !args.status,
            "new does not accept --database or --status"
        );
        ensure!(
            identifier(&name),
            "migration names use lowercase letters, digits and underscores"
        );
        std::fs::create_dir_all(&args.migrations)?;
        let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        let id = format!("{millis:020}_{name}");
        let path = args.migrations.join(format!("{id}.toml"));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        writeln!(
            file,
            "id = {id:?}\n# Replace the empty changes with explicit schema operations.\nchanges = []\n\n# Example (remove changes = [] above):\n# [[changes]]\n# action = \"create_table\"\n# [changes.table]\n# name = \"identity.accounts\"\n# columns = [{{ name = \"id\", kind = \"integer\" }}]\n# primary = [\"id\"]"
        )?;
        file.sync_all()?;
        println!("Created {}", path.display());
        return Ok(());
    }
    let database = args.database.context(
        "--database is required (for example: snap migrate --database .snap/store.sqlite)",
    )?;
    let migrations = read(&args.migrations)?;
    let report = if args.status {
        snap_sqlite::status(&database, &migrations)?
    } else {
        snap_sqlite::migrate(&database, &migrations)?
    };
    for id in &report.applied {
        println!("applied {id}");
    }
    for id in &report.pending {
        println!("pending {id}");
    }
    if !args.status && report.applied.is_empty() {
        println!("No pending migrations");
    }
    Ok(())
}
