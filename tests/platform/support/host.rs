//! Cartridge assembly shared by controlled and physical-IO setups.
use snap_document::{Registry, runtime::Runtime as Host, server::Document};
use snap_platform_tests::cartridge;
use snap_store::{Backend, Row, Store, migration::Migration};
use snap_transport::server::Config;
use std::sync::Arc;

pub fn migrations() -> Vec<Migration> {
    let mut migrations: Vec<Migration> = [snap_access::MIGRATION, snap_document::server::MIGRATION]
        .into_iter()
        .map(|text| toml::from_str(text).expect("valid module migration"))
        .collect();
    migrations.push(cartridge::migration());
    migrations.sort_by(|a, b| a.id.cmp(&b.id));
    migrations
}

/// Mount on a fresh Store. Probe rows remain cold so the host must prepare the
/// operation's declared data; this assembly never supplies execution responses.
pub fn mount<B: Backend>(
    mut store: Store<B>,
    config: Config,
    boot: String,
) -> Result<Host<B>, snap_store::Error> {
    for table in snap_access::TABLES
        .iter()
        .chain(snap_document::server::TABLES.iter())
    {
        store.load(table)?;
    }
    store.run("probe.seed", |tx| {
        for table in cartridge::TABLES {
            tx.insert(
                table,
                Row::from([("id".into(), 1.into()), ("value".into(), 0.into())]),
            )?;
        }
        Ok(())
    })?;
    for table in cartridge::TABLES {
        store.retain_keys(table, &Default::default())?;
    }
    let mut operations = snap_transport::operation::Registry::default();
    for definition in cartridge::definitions() {
        operations = operations.with_request(definition);
    }
    let host = Host::new(
        store,
        Arc::new(Document::new(
            Registry::new(vec![]).expect("empty document registry"),
        )),
        operations,
        Arc::new(snap_transport::bearer::Callbacks::new(Arc::new(
            |_, bearer| {
                if bearer == "alice" {
                    Ok("alice".into())
                } else {
                    Err(snap_store::Error::NotFound)
                }
            },
        ))),
        config,
        boot,
    );
    Ok(host)
}
