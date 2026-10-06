//! The same cartridge assembly as native hosts, with simulated dependency IO.
use super::host;
use snap_platform_tests::simulation::{CommitFault, Schedule, Simulation, Store, Timeline};
use snap_store::Catalog;
use snap_transport::host::Blocking;

pub fn setup(schedule: Schedule) -> (Simulation<Blocking<Store>>, Timeline, CommitFault) {
    let timeline = Timeline::new(schedule);
    let catalog = host::migrations()
        .iter()
        .try_fold(Catalog::default(), |catalog, migration| {
            migration.apply(&catalog)
        })
        .unwrap();
    let backend = Store::new(catalog.clone(), timeline.clone()).unwrap();
    let faults = backend.faults();
    let store = snap_store::Store::new(catalog, backend).unwrap();
    let host = host::mount(store, Default::default(), "simulation-boot-1".into()).unwrap();
    (Simulation::new(host, timeline.clone()), timeline, faults)
}
