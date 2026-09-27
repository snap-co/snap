use snap_access::{
    Access, Accessible, Actor, Audience, AuthorizationRequirement, ChangeSet, DirectGrant,
    GrantChange, KindDefinition, LinkChange, RegisterInitial, Resource, Role, TransferPolicy,
};
use snap_store::{Error, Store, Value};

fn migration() -> snap_store::migration::Migration {
    toml::from_str(snap_access::MIGRATION).unwrap()
}

fn notes_migration() -> snap_store::migration::Migration {
    toml::from_str(
        r#"
id = "0002_notes"

[[changes]]
action = "create_table"
[changes.table]
name = "test.notes"
primary = ["id"]
columns = [{ name = "id", kind = "text" }, { name = "body", kind = "text" }]
"#,
    )
    .unwrap()
}

fn store(loaded: bool) -> Store<snap_sqlite::Sqlite> {
    let mut store = snap_sqlite::Sqlite::memory(&[migration()]).unwrap();
    if loaded {
        for table in snap_access::TABLES {
            store.load(table).unwrap();
        }
    }
    store
}

fn store_with_notes() -> Store<snap_sqlite::Sqlite> {
    let mut store = snap_sqlite::Sqlite::memory(&[migration(), notes_migration()]).unwrap();
    for table in snap_access::TABLES.iter().chain(["test.notes"].iter()) {
        store.load(table).unwrap();
    }
    store
}

fn access() -> Access {
    Access::new(vec![
        KindDefinition::new("doc", TransferPolicy::Allowed).unwrap(),
        KindDefinition::new("fixed", TransferPolicy::Forbidden).unwrap(),
    ])
    .unwrap()
}

fn uuid(n: u32) -> String {
    format!("018f3c4b-6d2a-7000-8000-{:012x}", n)
}

fn res(kind: &str, n: u32) -> Resource {
    Resource::new(kind, &uuid(n)).unwrap()
}

fn grant(identity: &str, role: Role) -> DirectGrant {
    DirectGrant::new(identity, role).unwrap()
}

fn system_change() -> ChangeSet {
    ChangeSet::new(Actor::system())
}

fn grant_change(resource: Resource, identity: &str, role: Option<Role>) -> GrantChange {
    GrantChange {
        resource,
        identity: identity.into(),
        role,
    }
}

fn link(child: Resource, parent: Resource, linked: bool) -> LinkChange {
    LinkChange {
        child,
        parent,
        linked,
    }
}

fn setup_three(
    access: &Access,
    store: &mut Store<snap_sqlite::Sqlite>,
) -> (Resource, Resource, Resource) {
    let parent = res("doc", 1);
    let child = res("doc", 2);
    let grandchild = res("doc", 3);
    for (resource, audience) in [
        (&parent, Audience::Restricted),
        (&child, Audience::Restricted),
        (&grandchild, Audience::Restricted),
    ] {
        store
            .run("register", |tx| {
                access.register(tx, resource, audience, &[], None)
            })
            .unwrap();
    }
    (parent, child, grandchild)
}

#[test]
fn kind_registration_rejects_duplicates_and_bad_names() {
    assert!(KindDefinition::new("Bad_Name", TransferPolicy::Allowed).is_err());
    assert!(KindDefinition::new("", TransferPolicy::Allowed).is_err());
    assert!(
        Access::new(vec![
            KindDefinition::kind("doc").unwrap(),
            KindDefinition::kind("doc").unwrap(),
        ])
        .is_err()
    );
    let mut access = access();
    assert!(access.define(KindDefinition::kind("doc").unwrap()).is_err());
    assert!(
        access
            .define(KindDefinition::kind("extra").unwrap())
            .is_ok()
    );
    assert!(Resource::new("Bad", &uuid(1)).is_err());
    assert!(Resource::new("doc", "not-a-uuid").is_err());
}

#[test]
fn audiences_grant_viewers_only_to_intended_callers() {
    let access = access();
    let mut store = store(true);
    let restricted = res("doc", 10);
    let authenticated = res("doc", 11);
    let public = res("doc", 12);
    store
        .run("register", |tx| {
            access.register(tx, &restricted, Audience::Restricted, &[], None)?;
            access.register(tx, &authenticated, Audience::Authenticated, &[], None)?;
            access.register(tx, &public, Audience::Public, &[], None)?;
            Ok(())
        })
        .unwrap();
    let seen = store
        .run("roles", |tx| {
            Ok((
                access.role(tx, &restricted, Some("alice"), true)?,
                access.role(tx, &authenticated, Some("alice"), true)?,
                access.role(tx, &authenticated, None, true)?,
                access.role(tx, &public, None, true)?,
                access.role(tx, &public, Some("alice"), true)?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(
        seen,
        (
            None,
            Some(Role::Viewer),
            None,
            Some(Role::Viewer),
            Some(Role::Viewer)
        )
    );
    // Audience viewing is not authority: it cannot create links or satisfy owner checks.
    let denied = store.run("link", |tx| {
        let mut changes = ChangeSet::new(Actor::Identity("alice".into()));
        changes
            .links
            .push(link(public.clone(), restricted.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(denied, Err(Error::Invalid)));
    let authority = store
        .run("authority", |tx| {
            access.role(tx, &public, Some("alice"), false)
        })
        .unwrap()
        .value;
    assert_eq!(authority, None);
}

#[test]
fn direct_grants_preserve_the_full_ordered_role_ladder() {
    let access = access();
    let mut store = store(true);
    let resource = res("doc", 20);
    store
        .run("register", |tx| {
            access.register(tx, &resource, Audience::Restricted, &[], None)
        })
        .unwrap();
    for role in [Role::Viewer, Role::Editor, Role::Manager, Role::Owner] {
        store
            .run("grant", |tx| {
                let mut changes = system_change();
                changes
                    .grants
                    .push(grant_change(resource.clone(), "alice", Some(role)));
                access.change(tx, &changes)
            })
            .unwrap();
        let seen = store
            .run("read", |tx| {
                Ok((
                    access.direct_role(tx, &resource, "alice")?,
                    access.role(tx, &resource, Some("alice"), true)?,
                ))
            })
            .unwrap()
            .value;
        assert_eq!(seen, (Some(role), Some(role)));
    }
    let grants = store
        .run("grants", |tx| access.direct_grants(tx, &resource))
        .unwrap()
        .value;
    assert_eq!(grants, vec![grant("alice", Role::Owner)]);
}

#[test]
fn inheritance_passes_the_strongest_role_through_every_path() {
    let access = access();
    let mut store = store(true);
    let (parent_a, parent_b, child) = (res("doc", 30), res("doc", 31), res("doc", 32));
    let grandchild = res("doc", 33);
    for resource in [&parent_a, &parent_b, &child, &grandchild] {
        store
            .run("register", |tx| {
                access.register(tx, resource, Audience::Restricted, &[], None)
            })
            .unwrap();
    }
    store
        .run("setup", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(parent_a.clone(), "alice", Some(Role::Owner)));
            changes
                .grants
                .push(grant_change(parent_b.clone(), "alice", Some(Role::Editor)));
            changes
                .links
                .push(link(child.clone(), parent_a.clone(), true));
            changes
                .links
                .push(link(child.clone(), parent_b.clone(), true));
            changes
                .links
                .push(link(grandchild.clone(), child.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
    let seen = store
        .run("roles", |tx| {
            Ok((
                access.role(tx, &child, Some("alice"), true)?,
                access.role(tx, &grandchild, Some("alice"), true)?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(seen, (Some(Role::Owner), Some(Role::Owner)));

    // Removing the stronger path leaves the weaker inherited role.
    store
        .run("unlink", |tx| {
            let mut changes = system_change();
            changes
                .links
                .push(link(child.clone(), parent_a.clone(), false));
            access.change(tx, &changes)
        })
        .unwrap();
    let weakened = store
        .run("roles", |tx| access.role(tx, &child, Some("alice"), true))
        .unwrap()
        .value;
    assert_eq!(weakened, Some(Role::Editor));

    // Revoking the remaining direct grant removes the inherited role.
    store
        .run("revoke", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(parent_b.clone(), "alice", None));
            access.change(tx, &changes)
        })
        .unwrap();
    let gone = store
        .run("roles", |tx| {
            Ok((
                access.role(tx, &child, Some("alice"), true)?,
                access.role(tx, &grandchild, Some("alice"), true)?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(gone, (None, None));
}

#[test]
fn links_reject_missing_resources_self_links_and_cycles() {
    let access = access();
    let mut store = store(true);
    let (parent, child, grandchild) = setup_three(&access, &mut store);
    let missing = res("doc", 99);

    let missing_link = store.run("link", |tx| {
        let mut changes = system_change();
        changes
            .links
            .push(link(child.clone(), missing.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(missing_link, Err(Error::Invalid)));

    let self_link = store.run("link", |tx| {
        let mut changes = system_change();
        changes.links.push(link(child.clone(), child.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(self_link, Err(Error::Invalid)));

    store
        .run("link", |tx| {
            let mut changes = system_change();
            changes
                .links
                .push(link(child.clone(), parent.clone(), true));
            changes
                .links
                .push(link(grandchild.clone(), child.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
    let cycle = store.run("cycle", |tx| {
        let mut changes = system_change();
        changes
            .links
            .push(link(parent.clone(), grandchild.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(cycle, Err(Error::Invalid)));

    // A failed link rolls back its adjacent grant in the same change.
    let bundled = store.run("bundled", |tx| {
        let mut changes = system_change();
        changes
            .grants
            .push(grant_change(parent.clone(), "mallory", Some(Role::Viewer)));
        changes
            .links
            .push(link(grandchild.clone(), grandchild.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(bundled, Err(Error::Invalid)));
    let rolled_back = store
        .run("read", |tx| access.direct_role(tx, &parent, "mallory"))
        .unwrap()
        .value;
    assert_eq!(rolled_back, None);
}

#[test]
fn new_links_need_prechange_owner_authority() {
    let access = access();
    let mut store = store(true);
    let (parent_a, parent_b, child) = setup_three(&access, &mut store);
    let grandchild = res("doc", 43);
    store
        .run("register", |tx| {
            access.register(tx, &grandchild, Audience::Restricted, &[], None)
        })
        .unwrap();

    // A non-owner cannot create a link, even with editor or manager grants.
    store
        .run("grant", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(child.clone(), "alice", Some(Role::Editor)));
            access.change(tx, &changes)
        })
        .unwrap();
    for role in [Role::Editor, Role::Manager] {
        store
            .run("grant", |tx| {
                let mut changes = system_change();
                changes
                    .grants
                    .push(grant_change(grandchild.clone(), "alice", Some(role)));
                access.change(tx, &changes)
            })
            .unwrap();
        let denied = store.run("link", |tx| {
            let mut changes = ChangeSet::new(Actor::Identity("alice".into()));
            changes
                .links
                .push(link(grandchild.clone(), parent_b.clone(), true));
            access.change(tx, &changes)
        });
        assert!(
            matches!(denied, Err(Error::Invalid)),
            "{role:?} cannot link"
        );
    }

    // Same-change ownership is not pre-change authority, and the adjacent
    // grant rolls back with the denied link.
    let smuggled = store.run("smuggle", |tx| {
        let mut changes = ChangeSet::new(Actor::Identity("alice".into()));
        changes
            .grants
            .push(grant_change(grandchild.clone(), "alice", Some(Role::Owner)));
        changes
            .links
            .push(link(grandchild.clone(), parent_b.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(smuggled, Err(Error::Invalid)));
    let no_grant = store
        .run("read", |tx| access.direct_role(tx, &grandchild, "alice"))
        .unwrap()
        .value;
    // The earlier editor/manager grant from the loop above is still there, but
    // the smuggled owner grant must not have landed.
    assert_ne!(no_grant, Some(Role::Owner));

    // An owner can link; unlinks and reassertions need no authority.
    store
        .run("grant", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(child.clone(), "alice", Some(Role::Owner)));
            access.change(tx, &changes)
        })
        .unwrap();
    store
        .run("link", |tx| {
            let mut changes = ChangeSet::new(Actor::Identity("alice".into()));
            changes
                .links
                .push(link(child.clone(), parent_a.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
    store
        .run("unlink", |tx| {
            let mut changes = ChangeSet::new(Actor::Identity("alice".into()));
            changes
                .links
                .push(link(child.clone(), parent_a.clone(), false));
            access.change(tx, &changes)
        })
        .unwrap();
    let unlinked = store
        .run("read", |tx| {
            access.role(tx, &child, Some("reader"), true).map(|_| ())?;
            access.direct_role(tx, &child, "alice")
        })
        .unwrap()
        .value;
    assert_eq!(unlinked, Some(Role::Owner));

    // System actors bypass the link authority check.
    store
        .run("system-link", |tx| {
            let mut changes = system_change();
            changes
                .links
                .push(link(child.clone(), parent_a.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
}

#[test]
fn ownership_transfer_promotes_beneficiary_and_demotes_prior_owners() {
    let access = access();
    let mut store = store(true);
    let resource = res("doc", 50);
    store
        .run("register", |tx| {
            access.register(tx, &resource, Audience::Restricted, &[], None)
        })
        .unwrap();
    store
        .run("grant", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(resource.clone(), "alice", Some(Role::Owner)));
            changes
                .grants
                .push(grant_change(resource.clone(), "carol", Some(Role::Owner)));
            access.change(tx, &changes)
        })
        .unwrap();
    store
        .run("transfer", |tx| {
            access.transfer(tx, &resource, &Actor::Identity("alice".into()), "bob")
        })
        .unwrap();
    let seen = store
        .run("read", |tx| {
            Ok((
                access.direct_role(tx, &resource, "bob")?,
                access.direct_role(tx, &resource, "alice")?,
                access.direct_role(tx, &resource, "carol")?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(
        seen,
        (Some(Role::Owner), Some(Role::Manager), Some(Role::Manager))
    );

    // Managers cannot transfer.
    store
        .run("grant", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(resource.clone(), "dave", Some(Role::Manager)));
            access.change(tx, &changes)
        })
        .unwrap();
    let denied = store.run("transfer", |tx| {
        access.transfer(tx, &resource, &Actor::Identity("dave".into()), "erin")
    });
    assert!(matches!(denied, Err(Error::Invalid)));

    // Forbidden kinds reject even owner-initiated transfers without changes.
    let fixed = res("fixed", 51);
    store
        .run("register", |tx| {
            access.register(
                tx,
                &fixed,
                Audience::Restricted,
                &[grant("alice", Role::Owner)],
                None,
            )
        })
        .unwrap();
    let forbidden = store.run("transfer", |tx| {
        access.transfer(tx, &fixed, &Actor::Identity("alice".into()), "bob")
    });
    assert!(matches!(forbidden, Err(Error::Invalid)));
    let kept = store
        .run("read", |tx| access.direct_role(tx, &fixed, "alice"))
        .unwrap()
        .value;
    assert_eq!(kept, Some(Role::Owner));

    // Unknown resources cannot be transferred.
    let missing = res("doc", 52);
    let unknown = store.run("transfer", |tx| {
        access.transfer(tx, &missing, &Actor::system(), "bob")
    });
    assert!(matches!(unknown, Err(Error::Invalid)));
}

#[test]
fn authorization_requirements_gate_trusted_edits() {
    let access = access();
    let mut store = store(true);
    let resource = res("doc", 60);
    store
        .run("register", |tx| {
            access.register(tx, &resource, Audience::Restricted, &[], None)
        })
        .unwrap();
    store
        .run("grant", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(resource.clone(), "alice", Some(Role::Manager)));
            access.change(tx, &changes)
        })
        .unwrap();
    store
        .run("edit", |tx| {
            let mut changes = ChangeSet::new(Actor::Identity("mallory".into()));
            changes
                .authorization
                .push(AuthorizationRequirement::viewing(
                    resource.clone(),
                    "alice",
                    Role::Manager,
                ));
            changes
                .grants
                .push(grant_change(resource.clone(), "bob", Some(Role::Viewer)));
            access.change(tx, &changes)
        })
        .unwrap();
    let visible = store
        .run("read", |tx| access.role(tx, &resource, Some("bob"), true))
        .unwrap()
        .value;
    assert_eq!(visible, Some(Role::Viewer));

    // After revoking alice, the same precondition fails and stages nothing.
    store
        .run("revoke", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(resource.clone(), "alice", None));
            access.change(tx, &changes)
        })
        .unwrap();
    let denied = store.run("edit", |tx| {
        let mut changes = ChangeSet::new(Actor::Identity("mallory".into()));
        changes
            .authorization
            .push(AuthorizationRequirement::viewing(
                resource.clone(),
                "alice",
                Role::Manager,
            ));
        changes.grants.push(grant_change(
            resource.clone(),
            "mallory",
            Some(Role::Viewer),
        ));
        access.change(tx, &changes)
    });
    assert!(matches!(denied, Err(Error::Invalid)));
    let absent = store
        .run("read", |tx| access.direct_role(tx, &resource, "mallory"))
        .unwrap()
        .value;
    assert_eq!(absent, None);
}

#[test]
fn change_reports_exact_sorted_transitions() {
    let access = access();
    let mut store = store(true);
    let first = res("doc", 70);
    let second = res("doc", 71);
    for resource in [&first, &second] {
        store
            .run("register", |tx| {
                access.register(tx, resource, Audience::Restricted, &[], None)
            })
            .unwrap();
    }
    store
        .run("seed", |tx| {
            let mut changes = system_change();
            for resource in [&first, &second] {
                changes
                    .grants
                    .push(grant_change(resource.clone(), "bob", Some(Role::Editor)));
                changes
                    .grants
                    .push(grant_change(resource.clone(), "alice", Some(Role::Viewer)));
            }
            access.change(tx, &changes)
        })
        .unwrap();
    let result = store
        .run("change", |tx| {
            let mut changes = system_change();
            // Deliberately reversed: the summary must still sort by kind then id.
            changes
                .grants
                .push(grant_change(second.clone(), "carol", Some(Role::Manager)));
            changes
                .grants
                .push(grant_change(first.clone(), "carol", Some(Role::Manager)));
            access.change(tx, &changes)
        })
        .unwrap()
        .value;
    assert_eq!(result.grants.len(), 2);
    assert!(result.grants[0].resource.id <= result.grants[1].resource.id);
    for transition in &result.grants {
        assert_eq!(
            transition.before,
            vec![grant("alice", Role::Viewer), grant("bob", Role::Editor)]
        );
        assert_eq!(
            transition.after,
            vec![
                grant("alice", Role::Viewer),
                grant("bob", Role::Editor),
                grant("carol", Role::Manager),
            ]
        );
    }
    let empty = store
        .run("empty", |tx| access.change(tx, &system_change()))
        .unwrap()
        .value;
    assert!(empty.grants.is_empty());
}

#[test]
fn registration_handles_duplicates_initial_links_and_failed_validation() {
    let access = access();
    let mut store = store(true);
    let parent = res("doc", 80);
    store
        .run("register", |tx| {
            access.register(tx, &parent, Audience::Restricted, &[], None)
        })
        .unwrap();

    // Same kind and audience is idempotent.
    let again = store
        .run("register", |tx| {
            access.register(tx, &parent, Audience::Restricted, &[], None)
        })
        .unwrap()
        .value;
    assert!(!again);

    // Kind and audience are immutable.
    let mismatch = store.run("register", |tx| {
        access.register(tx, &parent, Audience::Public, &[], None)
    });
    assert!(matches!(mismatch, Err(Error::Invalid)));

    // Unknown kinds are rejected.
    let unknown = store.run("register", |tx| {
        access.register(
            tx,
            &Resource::new("nope", &uuid(81)).unwrap(),
            Audience::Restricted,
            &[],
            None,
        )
    });
    assert!(matches!(unknown, Err(Error::Invalid)));

    // Initial links bypass child authority but still require existence: the
    // new resource plus a stored parent link in one atomic registration.
    let fresh = res("doc", 82);
    let mut initial = RegisterInitial::new(Actor::Identity("mallory".into()));
    initial
        .links
        .push(link(fresh.clone(), parent.clone(), true));
    let created = store
        .run("register", |tx| {
            access.register(tx, &fresh, Audience::Restricted, &[], Some(&initial))
        })
        .unwrap()
        .value;
    assert!(created);
    let linked = store
        .run("read", |tx| access.orphaned(tx, &fresh))
        .unwrap()
        .value;
    assert!(!linked);

    // Initial links must involve the new resource and a stored parent.
    let invalid = res("doc", 83);
    let mut bad = RegisterInitial::new(Actor::system());
    bad.links.push(link(parent.clone(), res("doc", 84), true));
    let rejected = store.run("register", |tx| {
        access.register(tx, &invalid, Audience::Restricted, &[], Some(&bad))
    });
    assert!(matches!(rejected, Err(Error::Invalid)));
    let hidden = store
        .run("read", |tx| access.audience_of(tx, &invalid))
        .unwrap()
        .value;
    assert_eq!(hidden, None);
}

#[test]
fn remove_and_prune_report_deterministic_outcomes() {
    let access = access();
    let mut store = store(true);
    let resource = res("doc", 90);
    store
        .run("register", |tx| {
            access.register(
                tx,
                &resource,
                Audience::Restricted,
                &[grant("alice", Role::Owner), grant("bob", Role::Editor)],
                None,
            )
        })
        .unwrap();

    // An authorized removal returns the sorted pre-removal grants.
    let removed = store
        .run("remove", |tx| {
            access.remove(
                tx,
                &resource,
                Some(&AuthorizationRequirement::viewing(
                    resource.clone(),
                    "alice",
                    Role::Owner,
                )),
            )
        })
        .unwrap()
        .value;
    assert!(removed.removed);
    assert_eq!(
        removed.grants,
        vec![grant("alice", Role::Owner), grant("bob", Role::Editor)]
    );
    let missing = store
        .run("remove", |tx| access.remove(tx, &resource, None))
        .unwrap()
        .value;
    assert!(!missing.removed);

    // Prune only collects true orphans; linked or granted resources stay.
    let (parent, child) = (res("doc", 91), res("doc", 92));
    for resource in [&parent, &child] {
        store
            .run("register", |tx| {
                access.register(tx, resource, Audience::Restricted, &[], None)
            })
            .unwrap();
    }
    assert!(
        store
            .run("prune", |tx| access.prune(tx, &child))
            .unwrap()
            .value
    );
    assert!(
        !store
            .run("prune", |tx| access.prune(tx, &child))
            .unwrap()
            .value
    );
    let upload = res("doc", 93);
    store
        .run("register", |tx| {
            access.register(
                tx,
                &upload,
                Audience::Restricted,
                &[grant("alice", Role::Owner)],
                None,
            )
        })
        .unwrap();
    // A direct grant alone blocks orphan prune but not unlinked prune.
    assert!(
        !store
            .run("prune", |tx| access.prune(tx, &upload))
            .unwrap()
            .value
    );
    // Direct ownership alone does not stop an unlinked prune, but a parent
    // link does.
    assert!(
        store
            .run("prune-unlinked", |tx| access.prune_unlinked(tx, &upload))
            .unwrap()
            .value
    );
    let kept = res("doc", 94);
    store
        .run("register", |tx| {
            access.register(
                tx,
                &kept,
                Audience::Restricted,
                &[grant("alice", Role::Owner)],
                None,
            )
        })
        .unwrap();
    store
        .run("link", |tx| {
            let mut changes = system_change();
            changes.links.push(link(kept.clone(), parent.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
    assert!(
        !store
            .run("prune-unlinked", |tx| access.prune_unlinked(tx, &kept))
            .unwrap()
            .value
    );
    assert!(
        !store
            .run("prune", |tx| access.prune(tx, &kept))
            .unwrap()
            .value
    );
}

#[test]
fn wrong_kind_references_resolve_unknown_and_reject_writes() {
    let access = access();
    let mut store = store(true);
    let document = res("doc", 100);
    store
        .run("register", |tx| {
            access.register(
                tx,
                &document,
                Audience::Restricted,
                &[grant("alice", Role::Owner)],
                None,
            )
        })
        .unwrap();
    let blob = Resource::new("fixed", &uuid(100)).unwrap();
    // Same storage id would be a different kind; use a genuinely unknown kind
    // row instead: the doc id under another kind resolves as unknown.
    let wrong = Resource {
        id: document.id.clone(),
        kind: "fixed".into(),
    };
    let seen = store
        .run("read", |tx| {
            Ok((
                access.role(tx, &wrong, Some("alice"), true)?,
                access.direct_role(tx, &wrong, "alice")?,
                access.audience_of(tx, &wrong)?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(seen, (None, None, None));
    let _ = blob;
    let bad_link = store.run("link", |tx| {
        let mut changes = system_change();
        changes
            .links
            .push(link(wrong.clone(), document.clone(), true));
        access.change(tx, &changes)
    });
    assert!(matches!(bad_link, Err(Error::Invalid)));
    let bad_auth = store.run("change", |tx| {
        let mut changes = system_change();
        changes
            .authorization
            .push(AuthorizationRequirement::viewing(
                wrong.clone(),
                "alice",
                Role::Viewer,
            ));
        changes
            .grants
            .push(grant_change(document.clone(), "bob", Some(Role::Viewer)));
        access.change(tx, &changes)
    });
    assert!(matches!(bad_auth, Err(Error::Invalid)));
}

#[test]
fn orphaned_only_without_grants_or_parents_on_restricted() {
    let access = access();
    let mut store = store(true);
    let child = res("doc", 110);
    let parent = res("doc", 111);
    let open = res("doc", 112);
    store
        .run("register", |tx| {
            access.register(tx, &child, Audience::Restricted, &[], None)?;
            access.register(tx, &parent, Audience::Restricted, &[], None)?;
            access.register(tx, &open, Audience::Public, &[], None)?;
            Ok(())
        })
        .unwrap();
    let orphan = store
        .run("read", |tx| access.orphaned(tx, &child))
        .unwrap()
        .value;
    assert!(orphan);
    assert!(
        !store
            .run("read", |tx| access.orphaned(tx, &open))
            .unwrap()
            .value
    );
    store
        .run("link", |tx| {
            let mut changes = system_change();
            changes
                .links
                .push(link(child.clone(), parent.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
    assert!(
        !store
            .run("read", |tx| access.orphaned(tx, &child))
            .unwrap()
            .value
    );
    store
        .run("unlink", |tx| {
            let mut changes = system_change();
            changes
                .links
                .push(link(child.clone(), parent.clone(), false));
            changes
                .grants
                .push(grant_change(child.clone(), "alice", Some(Role::Viewer)));
            access.change(tx, &changes)
        })
        .unwrap();
    assert!(
        !store
            .run("read", |tx| access.orphaned(tx, &child))
            .unwrap()
            .value
    );
}

#[test]
fn accessible_enumeration_and_minimum_checks() {
    let access = access();
    let mut store = store(true);
    let parent = res("doc", 120);
    let child = res("doc", 121);
    let public = res("doc", 122);
    store
        .run("register", |tx| {
            access.register(tx, &parent, Audience::Restricted, &[], None)?;
            access.register(tx, &child, Audience::Restricted, &[], None)?;
            access.register(tx, &public, Audience::Public, &[], None)?;
            Ok(())
        })
        .unwrap();
    store
        .run("grant", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(parent.clone(), "alice", Some(Role::Editor)));
            changes
                .links
                .push(link(child.clone(), parent.clone(), true));
            access.change(tx, &changes)
        })
        .unwrap();
    let listed: Vec<Accessible> = store
        .run("list", |tx| access.accessible(tx, Some("alice"), true))
        .unwrap()
        .value;
    let ids: Vec<String> = listed
        .iter()
        .map(|entry| entry.resource.id.clone())
        .collect();
    assert!(ids.contains(&parent.id));
    assert!(ids.contains(&child.id));
    assert!(ids.contains(&public.id));
    let child_role = listed
        .iter()
        .find(|entry| entry.resource.id == child.id)
        .map(|entry| entry.role);
    assert_eq!(child_role, Some(Role::Editor));

    let anonymous: Vec<Accessible> = store
        .run("list", |tx| access.accessible(tx, None, true))
        .unwrap()
        .value;
    assert_eq!(anonymous.len(), 1);
    assert_eq!(anonymous[0].resource.id, public.id);

    let checks = store
        .run("check", |tx| {
            Ok((
                access.check(tx, &child, Some("alice"), Role::Viewer, true)?,
                access.check(tx, &child, Some("alice"), Role::Manager, true)?,
                access.check(tx, &public, None, Role::Viewer, true)?,
                access.check(tx, &parent, None, Role::Viewer, true)?,
            ))
        })
        .unwrap()
        .value;
    assert_eq!(checks, (true, false, true, false));

    // Direct listings carry kinds and only direct grants.
    let direct = store
        .run("direct", |tx| access.list_direct(tx, "alice"))
        .unwrap()
        .value;
    assert_eq!(direct.len(), 1);
    assert_eq!(direct[0].resource, parent);
}

#[test]
fn cold_tables_miss_and_discard_staged_work() {
    let access = access();
    let mut store = store(false);
    let resource = res("doc", 130);
    let cold = store.run("register", |tx| {
        access.register(tx, &resource, Audience::Restricted, &[], None)
    });
    assert!(matches!(cold, Err(Error::Miss(_))));
    assert_eq!(store.misses().count, 1);

    // A miss poisons the whole attempt even when the handler swallows it.
    let poisoned = store.run("poison", |tx| {
        let _ = access.role(tx, &resource, Some("alice"), true);
        Ok(())
    });
    assert!(matches!(poisoned, Err(Error::Miss(_))));
    for table in snap_access::TABLES {
        store.load(table).unwrap();
    }
    assert!(
        store
            .run("register", |tx| {
                access.register(tx, &resource, Audience::Restricted, &[], None)
            })
            .unwrap()
            .value
    );
    // The cold attempts staged nothing observable.
    let grants = store
        .run("read", |tx| access.direct_grants(tx, &resource))
        .unwrap()
        .value;
    assert!(grants.is_empty());
}

#[test]
fn access_and_caller_writes_commit_and_roll_back_together() {
    let access = access();
    let mut store = store_with_notes();
    let resource = res("doc", 140);
    store
        .run("register", |tx| {
            access.register(tx, &resource, Audience::Restricted, &[], None)
        })
        .unwrap();

    // Success commits both the grant and the caller-owned row atomically.
    store
        .run("joint", |tx| {
            let mut changes = system_change();
            changes
                .grants
                .push(grant_change(resource.clone(), "alice", Some(Role::Viewer)));
            access.change(tx, &changes)?;
            let mut row = snap_store::Row::new();
            row.insert("id".into(), Value::Text("note-1".into()));
            row.insert("body".into(), Value::Text("hello".into()));
            tx.insert("test.notes", row)?;
            Ok(())
        })
        .unwrap();
    let joint = store
        .run("read", |tx| {
            Ok((
                access.direct_role(tx, &resource, "alice")?,
                tx.get("test.notes", &[Value::Text("note-1".into())])?
                    .is_some(),
            ))
        })
        .unwrap()
        .value;
    assert_eq!(joint, (Some(Role::Viewer), true));

    // A caller failure after the Access edit discards both writes.
    let failed = store.run("abort", |tx| {
        let mut changes = system_change();
        changes
            .grants
            .push(grant_change(resource.clone(), "bob", Some(Role::Viewer)));
        access.change(tx, &changes)?;
        let mut row = snap_store::Row::new();
        row.insert("id".into(), Value::Text("note-2".into()));
        row.insert("body".into(), Value::Text("dropped".into()));
        tx.insert("test.notes", row)?;
        Err::<(), _>(Error::Unavailable)
    });
    assert!(matches!(failed, Err(Error::Unavailable)));
    let rolled_back = store
        .run("read", |tx| {
            Ok((
                access.direct_role(tx, &resource, "bob")?,
                tx.get("test.notes", &[Value::Text("note-2".into())])?
                    .is_some(),
            ))
        })
        .unwrap()
        .value;
    assert_eq!(rolled_back, (None, false));

    // A Store miss late in the attempt discards earlier Access and caller writes.
    let mut cold = snap_sqlite::Sqlite::memory(&[migration(), notes_migration()]).unwrap();
    let missed = cold.run("cold-joint", |tx| {
        let mut changes = system_change();
        changes
            .grants
            .push(grant_change(resource.clone(), "carol", Some(Role::Viewer)));
        access.change(tx, &changes)?;
        let mut row = snap_store::Row::new();
        row.insert("id".into(), Value::Text("note-3".into()));
        row.insert("body".into(), Value::Text("cold".into()));
        tx.insert("test.notes", row)?;
        Ok(())
    });
    assert!(matches!(missed, Err(Error::Miss(_))));
    for table in snap_access::TABLES.iter().chain(["test.notes"].iter()) {
        cold.load(table).unwrap();
    }
    let absent = cold
        .run("read", |tx| {
            Ok((
                access.direct_role(tx, &resource, "carol")?,
                tx.get("test.notes", &[Value::Text("note-3".into())])?
                    .is_some(),
            ))
        })
        .unwrap()
        .value;
    assert_eq!(absent, (None, false));
}

#[test]
fn access_migration_applies_cleanly() {
    let parsed = migration();
    assert_eq!(parsed.id, "0001_access");
    assert_eq!(parsed.changes.len(), 3);
    let mut store = snap_sqlite::Sqlite::memory(&[parsed]).unwrap();
    for table in snap_access::TABLES {
        store.load(table).unwrap();
    }
    let catalog = store.catalog();
    assert!(catalog.table("access.resources").is_ok());
    assert!(catalog.table("access.grants").is_ok());
    assert!(catalog.table("access.links").is_ok());
}
