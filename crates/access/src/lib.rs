//! Protected-resource authorization in the caller's Store transaction.
//!
//! Access keeps the original authorization semantics without its old storage,
//! residency, locking, or publication machinery. Portable callers share one
//! `snap_store::Transaction` across Access and their own module writes, so an
//! Access change and caller-owned rows commit atomically. Hosts publish any
//! derived invalidations only after `Store::run` returns `Committed`; a failed
//! transaction publishes nothing and stages nothing.
//!
//! Roles form a ladder: viewer < editor < manager < owner. A parent's role is
//! inherited unchanged by every descendant, and the strongest role across all
//! paths wins. Audience policy (`restricted`, `authenticated`, `public`) grants
//! viewer visibility only; it never confers authority for ownership transfers
//! or new links. Those edits require audience-free owner authority on the
//! child, checked against the pre-change graph: a mutation cannot grant itself
//! the authority it needs. System actors bypass that check; unlinks and
//! reassertions of an existing edge need no authority.
//!
//! All methods are synchronous, `no_std` with `alloc`, and perform no IO. They
//! take the caller's `&mut Transaction`, so Store misses propagate as
//! `Error::Miss` and poison the attempt like any other Store use: even a
//! handler that catches the error cannot commit. Hosts explicitly load the
//! Access tables, then callers may retry. Rejections (unknown kinds, wrong-kind
//! references, cycles, denied authority, forbidden transfers) return
//! `Error::Invalid`. Identity ids are opaque non-empty strings; the current
//! Identity backend issues 64-hex ids, so no UUID shape is enforced on them.
//! Resource ids keep the original UUID shape and kinds keep the lowercase
//! kebab namespace.
#![no_std]
extern crate alloc;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    string::{String, ToString},
    vec::Vec,
};
use snap_store::{Error, Row, Transaction, Value};

/// Ordered migration declarations for the Access tables.
pub const MIGRATION: &str = include_str!("../migrations/0001_access.toml");
/// Store tables owned by this module. Hosts load these to arrange residency.
pub const TABLES: [&str; 3] = ["access.resources", "access.grants", "access.links"];
const RESOURCES: &str = TABLES[0];
const GRANTS: &str = TABLES[1];
const LINKS: &str = TABLES[2];

/// Viewer < editor < manager < owner. Inheritance passes the source role
/// through unchanged; the strongest role across all paths wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer,
    Editor,
    Manager,
    Owner,
}

impl Role {
    /// Canonical row encoding.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Editor => "editor",
            Self::Manager => "manager",
            Self::Owner => "owner",
        }
    }

    /// Parse a row encoding. Unknown roles are data errors, never defaults.
    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "viewer" => Ok(Self::Viewer),
            "editor" => Ok(Self::Editor),
            "manager" => Ok(Self::Manager),
            "owner" => Ok(Self::Owner),
            _ => Err(Error::Invalid),
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Editor => 1,
            Self::Manager => 2,
            Self::Owner => 3,
        }
    }
}

/// True when the effective role meets the required minimum. A missing role
/// never satisfies any requirement.
pub fn allows(actual: Option<Role>, required: Role) -> bool {
    match actual {
        None => false,
        Some(role) => role.rank() >= required.rank(),
    }
}

/// Visibility policy. Audience-derived viewing is a viewer grant only and is
/// excluded from authority checks used for ownership and link edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    Restricted,
    Authenticated,
    Public,
}

impl Audience {
    /// Canonical row encoding.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Restricted => "restricted",
            Self::Authenticated => "authenticated",
            Self::Public => "public",
        }
    }

    /// Parse a row encoding.
    pub fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "restricted" => Ok(Self::Restricted),
            "authenticated" => Ok(Self::Authenticated),
            "public" => Ok(Self::Public),
            _ => Err(Error::Invalid),
        }
    }
}

/// Whether `transfer` may move ownership for a resource kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPolicy {
    Allowed,
    Forbidden,
}

/// Registered vocabulary for one family of protected resources. The creator is
/// always the owner; only the transfer policy varies per kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KindDefinition {
    pub name: String,
    pub transfer: TransferPolicy,
}

impl KindDefinition {
    /// Validate the kebab namespace. Duplicate registration fails at
    /// `Access::new` / `define`, mirroring the original boot check.
    pub fn new(name: &str, transfer: TransferPolicy) -> Result<Self, Error> {
        if !is_kind_name(name) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            name: name.into(),
            transfer,
        })
    }

    /// Area-0 default: ownership transfer allowed.
    pub fn kind(name: &str) -> Result<Self, Error> {
        Self::new(name, TransferPolicy::Allowed)
    }
}

/// Structural protected-resource reference: storage id plus expected kind.
/// Storage rows keep the bare id; callers pass this reference so operations
/// can validate kind. A kind mismatch is treated as unknown, never as a
/// cross-kind grant of authority.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Resource {
    pub id: String,
    pub kind: String,
}

impl Resource {
    /// Kinds require the lowercase kebab namespace; ids require UUID shape.
    /// Struct fields are re-validated on every call because literals bypass
    /// this constructor.
    pub fn new(kind: &str, id: &str) -> Result<Self, Error> {
        if !is_kind_name(kind) || !is_uuid(id) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            id: id.into(),
            kind: kind.into(),
        })
    }
}

/// Exact kind-and-id equality.
pub fn same_resource(left: &Resource, right: &Resource) -> bool {
    left.id == right.id && left.kind == right.kind
}

/// One direct grant, ordered by identity id in result summaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectGrant {
    pub identity: String,
    pub role: Role,
}

impl DirectGrant {
    pub fn new(identity: &str, role: Role) -> Result<Self, Error> {
        if identity.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            identity: identity.into(),
            role,
        })
    }
}

/// Kind-bearing resource with its role, used by direct and effective listings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accessible {
    pub resource: Resource,
    pub role: Role,
}

/// A direct-grant edit. `None` revokes. Revoking an absent grant is a no-op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantChange {
    pub resource: Resource,
    pub identity: String,
    pub role: Option<Role>,
}

/// A parent/child edge edit. Linking an existing edge and unlinking an absent
/// edge are idempotent no-ops. Unlinks never require authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkChange {
    pub child: Resource,
    pub parent: Resource,
    pub linked: bool,
}

/// A minimum-role precondition evaluated against the pre-change graph.
/// `audience` selects whether audience-derived viewing counts; authority
/// checks pass `false`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationRequirement {
    pub resource: Resource,
    pub identity: String,
    pub minimum: Role,
    pub audience: bool,
}

impl AuthorizationRequirement {
    /// Audience-derived viewing counts toward the minimum.
    pub fn viewing(resource: Resource, identity: &str, minimum: Role) -> Self {
        Self {
            resource,
            identity: identity.into(),
            minimum,
            audience: true,
        }
    }

    /// Audience-derived viewing does not count; use for authority checks.
    pub fn authority(resource: Resource, identity: &str, minimum: Role) -> Self {
        Self {
            resource,
            identity: identity.into(),
            minimum,
            audience: false,
        }
    }
}

/// Attributes trusted app-code changes and authorizes ownership transfers and
/// new links. System actors bypass the new-link and transfer owner checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Actor {
    System,
    Identity(String),
}

impl Actor {
    pub fn is_system(&self) -> bool {
        matches!(self, Self::System)
    }

    pub fn system() -> Self {
        Self::System
    }

    pub fn identity(identity: &str) -> Result<Self, Error> {
        if identity.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Self::Identity(identity.into()))
    }
}

/// One atomic grant/link publication with its preconditions. Identity actors
/// must hold audience-free effective owner authority on every newly linked
/// child in the pre-change graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    pub actor: Actor,
    pub authorization: Vec<AuthorizationRequirement>,
    pub grants: Vec<GrantChange>,
    pub links: Vec<LinkChange>,
}

impl ChangeSet {
    pub fn new(actor: Actor) -> Self {
        Self {
            actor,
            authorization: Vec::new(),
            grants: Vec::new(),
            links: Vec::new(),
        }
    }
}

/// Initial links (and preconditions) for a resource being registered. Unlike
/// `change`, registration does not demand pre-change child authority: the
/// child is new, so there is no prior owner. Every initial link must set
/// `linked` and involve the new resource, and both ends must exist once the
/// resource is inserted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterInitial {
    pub actor: Actor,
    pub authorization: Vec<AuthorizationRequirement>,
    pub links: Vec<LinkChange>,
}

impl RegisterInitial {
    pub fn new(actor: Actor) -> Self {
        Self {
            actor,
            authorization: Vec::new(),
            links: Vec::new(),
        }
    }
}

/// Exact direct-grant state surrounding one committed change. Transitions are
/// ordered by resource kind then id; both grant lists are ordered by identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantTransition {
    pub resource: Resource,
    pub before: Vec<DirectGrant>,
    pub after: Vec<DirectGrant>,
}

/// One transition for each resource touched by a direct-grant edit, in
/// deterministic kind-then-id order. An empty change reports no transitions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeResult {
    pub grants: Vec<GrantTransition>,
}

/// Removal outcome with the exact direct grants immediately before removal,
/// ordered by identity id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveResult {
    pub resource: Resource,
    pub removed: bool,
    pub grants: Vec<DirectGrant>,
}

/// Authorization over explicit kind vocabulary and Store-backed grants/links.
/// Kinds live in this struct (boot vocabulary, like the original layer
/// assembly); rows live in the caller's Store transaction.
pub struct Access {
    kinds: BTreeMap<String, TransferPolicy>,
}

impl Access {
    /// Assemble the kind vocabulary. Duplicate or malformed names fail boot.
    pub fn new(definitions: Vec<KindDefinition>) -> Result<Self, Error> {
        let mut kinds = BTreeMap::new();
        for definition in definitions {
            if !is_kind_name(&definition.name) || kinds.contains_key(&definition.name) {
                return Err(Error::Invalid);
            }
            kinds.insert(definition.name, definition.transfer);
        }
        Ok(Self { kinds })
    }

    /// Register one more kind. Duplicate names fail.
    pub fn define(&mut self, definition: KindDefinition) -> Result<(), Error> {
        if !is_kind_name(&definition.name) || self.kinds.contains_key(&definition.name) {
            return Err(Error::Invalid);
        }
        self.kinds.insert(definition.name, definition.transfer);
        Ok(())
    }

    /// Registered kind names in sorted order.
    pub fn kinds(&self) -> Vec<String> {
        self.kinds.keys().cloned().collect()
    }

    fn require_kind(&self, kind: &str) -> Result<TransferPolicy, Error> {
        if !is_kind_name(kind) {
            return Err(Error::Invalid);
        }
        self.kinds.get(kind).copied().ok_or(Error::Invalid)
    }

    /// Register a resource. The creator becomes owner only through explicit
    /// entries in `grants`; registration itself confers no implicit grant.
    /// Returns `false` when the resource already exists with the same kind and
    /// audience (no writes staged). A kind/audience mismatch on an existing id
    /// is rejected: both are immutable. Unknown kinds are rejected.
    pub fn register(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
        audience: Audience,
        grants: &[DirectGrant],
        initial: Option<&RegisterInitial>,
    ) -> Result<bool, Error> {
        validate_resource(resource)?;
        self.require_kind(&resource.kind)?;
        let staged: BTreeMap<String, Role> = dedupe_grants(grants)?;
        if let Some(init) = initial {
            validate_actor(&init.actor)?;
            for requirement in &init.authorization {
                validate_resource(&requirement.resource)?;
                validate_identity(&requirement.identity)?;
            }
            for link in &init.links {
                validate_resource(&link.child)?;
                validate_resource(&link.parent)?;
            }
        }
        let graph = load_graph(tx)?;
        if let Some((kind, current)) = graph.resources.get(&resource.id) {
            if *kind == resource.kind && *current == audience {
                return Ok(false);
            }
            return Err(Error::Invalid);
        }
        if let Some(init) = initial {
            if !init.authorization.is_empty() {
                check_requirements(&graph, &init.authorization)?;
            }
            let mut working = graph.clone();
            working
                .resources
                .insert(resource.id.clone(), (resource.kind.clone(), audience));
            for link in &init.links {
                if !link.linked
                    || (!same_resource(&link.child, resource)
                        && !same_resource(&link.parent, resource))
                {
                    return Err(Error::Invalid);
                }
                for end in [&link.child, &link.parent] {
                    if same_resource(end, resource) {
                        continue;
                    }
                    match working.resources.get(&end.id) {
                        Some((kind, _)) if *kind == end.kind => {}
                        _ => return Err(Error::Invalid),
                    }
                }
                if has_edge(&working, &link.child.id, &link.parent.id)
                    || would_cycle(&working, &link.child.id, &link.parent.id)
                {
                    return Err(Error::Invalid);
                }
                add_edge(&mut working, &link.child.id, &link.parent.id);
            }
        }
        let mut resource_row = Row::new();
        resource_row.insert("id".into(), Value::Text(resource.id.clone()));
        resource_row.insert("kind".into(), Value::Text(resource.kind.clone()));
        resource_row.insert(
            "audience".into(),
            Value::Text(audience.as_str().to_string()),
        );
        tx.insert(RESOURCES, resource_row)?;
        for (identity, role) in &staged {
            let mut row = Row::new();
            row.insert("resource".into(), Value::Text(resource.id.clone()));
            row.insert("identity".into(), Value::Text(identity.clone()));
            row.insert("role".into(), Value::Text(role.as_str().to_string()));
            tx.insert(GRANTS, row)?;
        }
        if let Some(init) = initial {
            for link in &init.links {
                let mut row = Row::new();
                row.insert("child".into(), Value::Text(link.child.id.clone()));
                row.insert("parent".into(), Value::Text(link.parent.id.clone()));
                tx.insert(LINKS, row)?;
            }
        }
        Ok(true)
    }

    /// Atomically check pre-change authorization, then apply grant revocations,
    /// assignments, and link edges. New-link authority and every authorization
    /// requirement are evaluated against the pre-change graph; a grant staged
    /// earlier in this same change never authorizes a link later in it. Any
    /// rejection stages nothing observable: the handler returns `Invalid` and
    /// Store discards the scratch state.
    pub fn change(
        &self,
        tx: &mut Transaction<'_>,
        changes: &ChangeSet,
    ) -> Result<ChangeResult, Error> {
        validate_actor(&changes.actor)?;
        for requirement in &changes.authorization {
            validate_resource(&requirement.resource)?;
            validate_identity(&requirement.identity)?;
        }
        for grant in &changes.grants {
            validate_resource(&grant.resource)?;
            validate_identity(&grant.identity)?;
            self.require_kind(&grant.resource.kind)?;
        }
        for link in &changes.links {
            validate_resource(&link.child)?;
            validate_resource(&link.parent)?;
        }
        let mut graph = load_graph(tx)?;
        if !changes.authorization.is_empty() {
            check_requirements(&graph, &changes.authorization)?;
        }
        if let Actor::Identity(actor) = &changes.actor {
            let authority = compile(&graph, Some(actor.as_str()), false);
            for link in &changes.links {
                if !link.linked || has_edge(&graph, &link.child.id, &link.parent.id) {
                    continue;
                }
                if role_in(&authority, &graph, &link.child) != Some(Role::Owner) {
                    return Err(Error::Invalid);
                }
            }
        }
        let mut touched: BTreeMap<(String, String), Resource> = BTreeMap::new();
        for grant in &changes.grants {
            touched.insert(
                (grant.resource.kind.clone(), grant.resource.id.clone()),
                grant.resource.clone(),
            );
        }
        let touched: Vec<Resource> = touched.into_values().collect();
        let mut before: BTreeMap<String, Vec<DirectGrant>> = BTreeMap::new();
        for resource in &touched {
            before.insert(resource.id.clone(), sorted_direct(&graph, &resource.id));
        }
        for grant in &changes.grants {
            match graph.resources.get(&grant.resource.id) {
                Some((kind, _)) if *kind == grant.resource.kind => {}
                _ => return Err(Error::Invalid),
            }
            let key = [
                Value::Text(grant.resource.id.clone()),
                Value::Text(grant.identity.clone()),
            ];
            match grant.role {
                Some(role) => {
                    if tx.get(GRANTS, &key)?.is_some() {
                        let mut delta = Row::new();
                        delta.insert("role".into(), Value::Text(role.as_str().to_string()));
                        tx.update(GRANTS, &key, delta)?;
                    } else {
                        let mut row = Row::new();
                        row.insert("resource".into(), Value::Text(grant.resource.id.clone()));
                        row.insert("identity".into(), Value::Text(grant.identity.clone()));
                        row.insert("role".into(), Value::Text(role.as_str().to_string()));
                        tx.insert(GRANTS, row)?;
                    }
                    graph
                        .grants
                        .entry(grant.resource.id.clone())
                        .or_default()
                        .insert(grant.identity.clone(), role);
                }
                None => {
                    if tx.get(GRANTS, &key)?.is_some() {
                        tx.delete(GRANTS, &key)?;
                    }
                    if let Some(map) = graph.grants.get_mut(&grant.resource.id) {
                        map.remove(&grant.identity);
                        if map.is_empty() {
                            graph.grants.remove(&grant.resource.id);
                        }
                    }
                }
            }
        }
        for link in &changes.links {
            let child_ok = matches!(graph.resources.get(&link.child.id), Some((kind, _)) if *kind == link.child.kind);
            let parent_ok = matches!(graph.resources.get(&link.parent.id), Some((kind, _)) if *kind == link.parent.kind);
            if !child_ok || !parent_ok {
                return Err(Error::Invalid);
            }
            let key = [
                Value::Text(link.child.id.clone()),
                Value::Text(link.parent.id.clone()),
            ];
            if link.linked {
                if has_edge(&graph, &link.child.id, &link.parent.id) {
                    continue;
                }
                if would_cycle(&graph, &link.child.id, &link.parent.id) {
                    return Err(Error::Invalid);
                }
                let mut row = Row::new();
                row.insert("child".into(), Value::Text(link.child.id.clone()));
                row.insert("parent".into(), Value::Text(link.parent.id.clone()));
                tx.insert(LINKS, row)?;
                add_edge(&mut graph, &link.child.id, &link.parent.id);
            } else {
                if !has_edge(&graph, &link.child.id, &link.parent.id) {
                    continue;
                }
                tx.delete(LINKS, &key)?;
                remove_edge(&mut graph, &link.child.id, &link.parent.id);
            }
        }
        Ok(ChangeResult {
            grants: touched
                .iter()
                .map(|resource| GrantTransition {
                    resource: resource.clone(),
                    before: before.remove(&resource.id).unwrap_or_default(),
                    after: sorted_direct(&graph, &resource.id),
                })
                .collect(),
        })
    }

    /// Move ownership to a beneficiary under the kind's transfer policy. Every
    /// other prior owner is demoted to manager. Ownership cannot be
    /// relinquished into the void: a beneficiary is required. Identity actors
    /// must already own the resource in the pre-change graph; system actors
    /// bypass that check. Transfer-forbidden kinds reject all transfers.
    pub fn transfer(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
        actor: &Actor,
        beneficiary: &str,
    ) -> Result<ChangeResult, Error> {
        validate_resource(resource)?;
        validate_actor(actor)?;
        validate_identity(beneficiary)?;
        if self.require_kind(&resource.kind)? != TransferPolicy::Allowed {
            return Err(Error::Invalid);
        }
        let graph = load_graph(tx)?;
        match graph.resources.get(&resource.id) {
            Some((kind, _)) if *kind == resource.kind => {}
            _ => return Err(Error::Invalid),
        }
        if let Actor::Identity(identity) = actor {
            let current = compile(&graph, Some(identity.as_str()), true);
            if role_in(&current, &graph, resource) != Some(Role::Owner) {
                return Err(Error::Invalid);
            }
        }
        let before = sorted_direct(&graph, &resource.id);
        let mut desired: BTreeMap<String, Role> =
            graph.grants.get(&resource.id).cloned().unwrap_or_default();
        desired.insert(beneficiary.to_string(), Role::Owner);
        for (identity, role) in desired.clone() {
            if identity != beneficiary && role == Role::Owner {
                desired.insert(identity, Role::Manager);
            }
        }
        let current: BTreeMap<String, Role> =
            graph.grants.get(&resource.id).cloned().unwrap_or_default();
        for (identity, role) in &desired {
            if current.get(identity) == Some(role) {
                continue;
            }
            let key = [
                Value::Text(resource.id.clone()),
                Value::Text(identity.clone()),
            ];
            if current.contains_key(identity) {
                let mut delta = Row::new();
                delta.insert("role".into(), Value::Text(role.as_str().to_string()));
                tx.update(GRANTS, &key, delta)?;
            } else {
                let mut row = Row::new();
                row.insert("resource".into(), Value::Text(resource.id.clone()));
                row.insert("identity".into(), Value::Text(identity.clone()));
                row.insert("role".into(), Value::Text(role.as_str().to_string()));
                tx.insert(GRANTS, row)?;
            }
        }
        let after = desired
            .into_iter()
            .map(|(identity, role)| DirectGrant { identity, role })
            .collect::<Vec<_>>();
        Ok(ChangeResult {
            grants: alloc::vec![GrantTransition {
                resource: resource.clone(),
                before,
                after,
            }],
        })
    }

    /// Remove a resource with all of its direct grants and parent/child edges.
    /// An optional authorization requirement is checked against the pre-change
    /// graph. Missing or wrong-kind resources report `removed: false` without
    /// staging writes. Children that lose their final parent become orphaned
    /// (or unlinked); Access never auto-collects them — resource modules
    /// decide, e.g. via `prune`.
    pub fn remove(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
        authorization: Option<&AuthorizationRequirement>,
    ) -> Result<RemoveResult, Error> {
        validate_resource(resource)?;
        if let Some(requirement) = authorization {
            validate_resource(&requirement.resource)?;
            validate_identity(&requirement.identity)?;
        }
        let graph = load_graph(tx)?;
        if let Some(requirement) = authorization {
            check_requirements(&graph, alloc::slice::from_ref(requirement))?;
        }
        match graph.resources.get(&resource.id) {
            Some((kind, _)) if *kind == resource.kind => {}
            _ => {
                return Ok(RemoveResult {
                    resource: resource.clone(),
                    removed: false,
                    grants: Vec::new(),
                });
            }
        }
        let grants = sorted_direct(&graph, &resource.id);
        delete_resource(tx, &graph, &resource.id)?;
        Ok(RemoveResult {
            resource: resource.clone(),
            removed: true,
            grants,
        })
    }

    /// Explicit landfill prune: removes the resource only when it remains
    /// orphaned (restricted with no direct grants and no parent links).
    /// Access never auto-collects; resource modules decide when to prune.
    pub fn prune(&self, tx: &mut Transaction<'_>, resource: &Resource) -> Result<bool, Error> {
        validate_resource(resource)?;
        let graph = load_graph(tx)?;
        if !is_orphaned(&graph, resource) {
            return Ok(false);
        }
        delete_resource(tx, &graph, &resource.id)?;
        Ok(true)
    }

    /// Remove a resource only when it has no parent links. Direct ownership is
    /// ignored; use `prune` when grants must also be absent. Used by delayed
    /// attachment sweeps.
    pub fn prune_unlinked(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
    ) -> Result<bool, Error> {
        validate_resource(resource)?;
        let graph = load_graph(tx)?;
        match graph.resources.get(&resource.id) {
            Some((kind, _)) if *kind == resource.kind => {}
            _ => return Ok(false),
        }
        if graph
            .parents
            .get(&resource.id)
            .is_some_and(|p| !p.is_empty())
        {
            return Ok(false);
        }
        delete_resource(tx, &graph, &resource.id)?;
        Ok(true)
    }

    /// Check minimum-role preconditions without editing Access. Every denial
    /// returns `Invalid`, so callers combining this with their own writes get
    /// atomic rollback for free.
    pub fn authorize(
        &self,
        tx: &mut Transaction<'_>,
        requirements: &[AuthorizationRequirement],
    ) -> Result<(), Error> {
        for requirement in requirements {
            validate_resource(&requirement.resource)?;
            validate_identity(&requirement.identity)?;
        }
        let graph = load_graph(tx)?;
        check_requirements(&graph, requirements)
    }

    /// Effective role including inheritance and, when `audience` is set,
    /// audience-derived viewing. Pass `false` for the authority used by
    /// ownership and link edits. Missing or wrong-kind resources resolve to
    /// `None` (known absence); insufficient residency is a `Miss`, never `None`.
    pub fn role(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
        identity: Option<&str>,
        audience: bool,
    ) -> Result<Option<Role>, Error> {
        validate_resource(resource)?;
        if let Some(identity) = identity {
            validate_identity(identity)?;
        }
        let graph = load_graph(tx)?;
        Ok(role_in(
            &compile(&graph, identity, audience),
            &graph,
            resource,
        ))
    }

    /// Minimum-role check over `role`. Anonymous callers supply `None` and can
    /// only ever satisfy viewer on public resources when `audience` is set.
    pub fn check(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
        identity: Option<&str>,
        minimum: Role,
        audience: bool,
    ) -> Result<bool, Error> {
        Ok(allows(
            self.role(tx, resource, identity, audience)?,
            minimum,
        ))
    }

    /// Exact direct grant without inheritance or audience. Missing or
    /// wrong-kind resources resolve to `None`.
    pub fn direct_role(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
        identity: &str,
    ) -> Result<Option<Role>, Error> {
        validate_resource(resource)?;
        validate_identity(identity)?;
        match tx.get(RESOURCES, &[Value::Text(resource.id.clone())])? {
            Some(row) if row_kind(&row)? == resource.kind => {}
            _ => return Ok(None),
        }
        match tx.get(
            GRANTS,
            &[
                Value::Text(resource.id.clone()),
                Value::Text(identity.into()),
            ],
        )? {
            Some(row) => Ok(Some(row_role(&row)?)),
            None => Ok(None),
        }
    }

    /// Every direct grant on a resource, ordered by identity id. Missing or
    /// wrong-kind resources report an empty list.
    pub fn direct_grants(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
    ) -> Result<Vec<DirectGrant>, Error> {
        validate_resource(resource)?;
        match tx.get(RESOURCES, &[Value::Text(resource.id.clone())])? {
            Some(row) if row_kind(&row)? == resource.kind => {}
            _ => return Ok(Vec::new()),
        }
        let rows = tx.find(GRANTS, "primary", &[Value::Text(resource.id.clone())])?;
        let mut grants = Vec::with_capacity(rows.len());
        for row in &rows {
            grants.push(DirectGrant {
                identity: row_identity(row)?.into(),
                role: row_role(row)?,
            });
        }
        grants.sort_by(|left, right| left.identity.cmp(&right.identity));
        Ok(grants)
    }

    /// Kind-bearing direct grants for one identity, ordered by kind then id.
    pub fn list_direct(
        &self,
        tx: &mut Transaction<'_>,
        identity: &str,
    ) -> Result<Vec<Accessible>, Error> {
        validate_identity(identity)?;
        let rows = tx.find(GRANTS, "identity", &[Value::Text(identity.into())])?;
        let mut listed = Vec::with_capacity(rows.len());
        for row in &rows {
            let id = row_resource(row)?;
            let role = row_role(row)?;
            let Some(record) = tx.get(RESOURCES, &[Value::Text(id.into())])? else {
                continue;
            };
            listed.push(Accessible {
                resource: Resource {
                    id: id.into(),
                    kind: row_kind(&record)?,
                },
                role,
            });
        }
        listed.sort_by(|left, right| {
            left.resource
                .kind
                .cmp(&right.resource.kind)
                .then(left.resource.id.cmp(&right.resource.id))
        });
        Ok(listed)
    }

    /// Stored audience policy. Missing or wrong-kind resources resolve to `None`.
    pub fn audience_of(
        &self,
        tx: &mut Transaction<'_>,
        resource: &Resource,
    ) -> Result<Option<Audience>, Error> {
        validate_resource(resource)?;
        match tx.get(RESOURCES, &[Value::Text(resource.id.clone())])? {
            Some(row) if row_kind(&row)? == resource.kind => Ok(Some(row_audience(&row)?)),
            _ => Ok(None),
        }
    }

    /// True when a restricted resource has no direct grants and no parent
    /// links. Missing, wrong-kind, and non-restricted resources are not
    /// orphaned.
    pub fn orphaned(&self, tx: &mut Transaction<'_>, resource: &Resource) -> Result<bool, Error> {
        validate_resource(resource)?;
        let graph = load_graph(tx)?;
        Ok(is_orphaned(&graph, resource))
    }

    /// Every resource the caller may read, with effective roles, ordered by
    /// kind then id. This is the loading manifest input: gaining access adds
    /// desired holdings, losing it removes them. Anonymous callers see only
    /// public resources, and only when `audience` is set.
    pub fn accessible(
        &self,
        tx: &mut Transaction<'_>,
        identity: Option<&str>,
        audience: bool,
    ) -> Result<Vec<Accessible>, Error> {
        if let Some(identity) = identity {
            validate_identity(identity)?;
        }
        let graph = load_graph(tx)?;
        let compiled = compile(&graph, identity, audience);
        let mut listed = Vec::with_capacity(compiled.len());
        for (id, role) in &compiled {
            let Some((kind, _)) = graph.resources.get(id) else {
                continue;
            };
            listed.push(Accessible {
                resource: Resource {
                    id: id.clone(),
                    kind: kind.clone(),
                },
                role: *role,
            });
        }
        listed.sort_by(|left, right| {
            left.resource
                .kind
                .cmp(&right.resource.kind)
                .then(left.resource.id.cmp(&right.resource.id))
        });
        Ok(listed)
    }
}

fn validate_resource(resource: &Resource) -> Result<(), Error> {
    if !is_kind_name(&resource.kind) || !is_uuid(&resource.id) {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn validate_identity(identity: &str) -> Result<(), Error> {
    if identity.is_empty() {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn validate_actor(actor: &Actor) -> Result<(), Error> {
    if let Actor::Identity(identity) = actor {
        validate_identity(identity)?;
    }
    Ok(())
}

/// Lowercase kebab namespace: `^[a-z][a-z0-9-]*$`.
fn is_kind_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    match bytes.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }
    bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// UUID shape in any version: 8-4-4-4-12 lowercase or uppercase hex.
fn is_uuid(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn dedupe_grants(grants: &[DirectGrant]) -> Result<BTreeMap<String, Role>, Error> {
    let mut staged = BTreeMap::new();
    for grant in grants {
        validate_identity(&grant.identity)?;
        staged.insert(grant.identity.clone(), grant.role);
    }
    Ok(staged)
}

#[derive(Clone, Default)]
struct Graph {
    resources: BTreeMap<String, (String, Audience)>,
    grants: BTreeMap<String, BTreeMap<String, Role>>,
    parents: BTreeMap<String, Vec<String>>,
    children: BTreeMap<String, Vec<String>>,
}

/// Load the complete Access image. Secondary-index and full scans require
/// complete residency, so a partially loaded table reports a `Miss` instead of
/// a silently incomplete graph. Callers must load `TABLES` first.
fn load_graph(tx: &mut Transaction<'_>) -> Result<Graph, Error> {
    let mut graph = Graph::default();
    for row in tx.find(RESOURCES, "primary", &[])? {
        let id = row_text(&row, "id")?.to_string();
        let kind = row_text(&row, "kind")?.to_string();
        let audience = Audience::parse(row_text(&row, "audience")?)?;
        if !is_kind_name(&kind) || !is_uuid(&id) || graph.resources.contains_key(&id) {
            return Err(Error::Invalid);
        }
        graph.resources.insert(id, (kind, audience));
    }
    for row in tx.find(GRANTS, "primary", &[])? {
        let resource = row_text(&row, "resource")?.to_string();
        let identity = row_text(&row, "identity")?.to_string();
        let role = Role::parse(row_text(&row, "role")?)?;
        if identity.is_empty() {
            return Err(Error::Invalid);
        }
        graph
            .grants
            .entry(resource)
            .or_default()
            .insert(identity, role);
    }
    for row in tx.find(LINKS, "primary", &[])? {
        let child = row_text(&row, "child")?.to_string();
        let parent = row_text(&row, "parent")?.to_string();
        add_edge(&mut graph, &child, &parent);
    }
    Ok(graph)
}

fn add_edge(graph: &mut Graph, child: &str, parent: &str) {
    graph
        .parents
        .entry(child.to_string())
        .or_default()
        .push(parent.to_string());
    graph
        .children
        .entry(parent.to_string())
        .or_default()
        .push(child.to_string());
}

fn remove_edge(graph: &mut Graph, child: &str, parent: &str) {
    if let Some(parents) = graph.parents.get_mut(child) {
        parents.retain(|candidate| candidate != parent);
        if parents.is_empty() {
            graph.parents.remove(child);
        }
    }
    if let Some(children) = graph.children.get_mut(parent) {
        children.retain(|candidate| candidate != child);
        if children.is_empty() {
            graph.children.remove(parent);
        }
    }
}

fn has_edge(graph: &Graph, child: &str, parent: &str) -> bool {
    graph
        .parents
        .get(child)
        .is_some_and(|parents| parents.iter().any(|candidate| candidate == parent))
}

/// True on self-links and whenever the parent already reaches the child
/// through parent links. Checked incrementally, so multi-edge changes in one
/// transaction cannot smuggle a cycle through ordering.
fn would_cycle(graph: &Graph, child: &str, parent: &str) -> bool {
    if child == parent {
        return true;
    }
    let mut stack: Vec<String> = alloc::vec![parent.to_string()];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    while let Some(id) = stack.pop() {
        if id == child {
            return true;
        }
        if seen.insert(id.clone())
            && let Some(parents) = graph.parents.get(&id)
        {
            stack.extend(parents.iter().cloned());
        }
    }
    false
}

fn push_role(
    compiled: &mut BTreeMap<String, Role>,
    pending: &mut Vec<String>,
    id: &str,
    role: Role,
) {
    let stronger = match compiled.get(id) {
        None => true,
        Some(prior) => prior.rank() < role.rank(),
    };
    if stronger {
        compiled.insert(id.to_string(), role);
        pending.push(id.to_string());
    }
}

/// Compile effective roles for one caller. Audience seeds and direct grants
/// enter the worklist first, then propagate parent-to-child until every path
/// has offered its role. Convergence is monotone (only strengthening), so
/// queue order cannot affect the strongest-role outcome.
fn compile(graph: &Graph, identity: Option<&str>, audience: bool) -> BTreeMap<String, Role> {
    let mut compiled: BTreeMap<String, Role> = BTreeMap::new();
    let mut pending: Vec<String> = Vec::new();
    if audience {
        for (id, (_, policy)) in &graph.resources {
            match policy {
                Audience::Public => push_role(&mut compiled, &mut pending, id, Role::Viewer),
                Audience::Authenticated if identity.is_some() => {
                    push_role(&mut compiled, &mut pending, id, Role::Viewer);
                }
                Audience::Authenticated | Audience::Restricted => {}
            }
        }
    }
    if let Some(who) = identity {
        for (resource, map) in &graph.grants {
            if let Some(role) = map.get(who)
                && graph.resources.contains_key(resource)
            {
                push_role(&mut compiled, &mut pending, resource, *role);
            }
        }
    }
    while let Some(parent) = pending.pop() {
        let Some(role) = compiled.get(&parent).copied() else {
            continue;
        };
        let Some(children) = graph.children.get(&parent) else {
            continue;
        };
        for child in children.clone() {
            if graph.resources.contains_key(&child) {
                push_role(&mut compiled, &mut pending, &child, role);
            }
        }
    }
    compiled
}

/// Kind-aware lookup: a kind mismatch resolves to unknown, never to a
/// cross-kind grant of authority.
fn role_in(compiled: &BTreeMap<String, Role>, graph: &Graph, resource: &Resource) -> Option<Role> {
    match graph.resources.get(&resource.id) {
        Some((kind, _)) if *kind == resource.kind => compiled.get(&resource.id).copied(),
        _ => None,
    }
}

fn check_requirements(
    graph: &Graph,
    requirements: &[AuthorizationRequirement],
) -> Result<(), Error> {
    let mut cache: BTreeMap<(String, bool), BTreeMap<String, Role>> = BTreeMap::new();
    for requirement in requirements {
        let key = (requirement.identity.clone(), requirement.audience);
        let compiled = match cache.get(&key) {
            Some(compiled) => compiled.clone(),
            None => {
                let compiled = compile(
                    graph,
                    Some(requirement.identity.as_str()),
                    requirement.audience,
                );
                cache.insert(key, compiled.clone());
                compiled
            }
        };
        if !allows(
            role_in(&compiled, graph, &requirement.resource),
            requirement.minimum,
        ) {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}

fn sorted_direct(graph: &Graph, resource: &str) -> Vec<DirectGrant> {
    let mut grants: Vec<DirectGrant> = graph
        .grants
        .get(resource)
        .map(|map| {
            map.iter()
                .map(|(identity, role)| DirectGrant {
                    identity: identity.clone(),
                    role: *role,
                })
                .collect()
        })
        .unwrap_or_default();
    grants.sort_by(|left, right| left.identity.cmp(&right.identity));
    grants
}

fn is_orphaned(graph: &Graph, resource: &Resource) -> bool {
    match graph.resources.get(&resource.id) {
        Some((kind, audience)) if *kind == resource.kind && *audience == Audience::Restricted => {}
        _ => return false,
    }
    if graph
        .grants
        .get(&resource.id)
        .is_some_and(|map| !map.is_empty())
    {
        return false;
    }
    if graph
        .parents
        .get(&resource.id)
        .is_some_and(|parents| !parents.is_empty())
    {
        return false;
    }
    true
}

fn delete_resource(tx: &mut Transaction<'_>, graph: &Graph, id: &str) -> Result<(), Error> {
    if let Some(map) = graph.grants.get(id) {
        for identity in map.keys() {
            tx.delete(
                GRANTS,
                &[Value::Text(id.to_string()), Value::Text(identity.clone())],
            )?;
        }
    }
    if let Some(parents) = graph.parents.get(id) {
        for parent in parents.clone() {
            tx.delete(LINKS, &[Value::Text(id.to_string()), Value::Text(parent)])?;
        }
    }
    if let Some(children) = graph.children.get(id) {
        for child in children.clone() {
            tx.delete(LINKS, &[Value::Text(child), Value::Text(id.to_string())])?;
        }
    }
    tx.delete(RESOURCES, &[Value::Text(id.to_string())])?;
    Ok(())
}

fn row_text<'a>(row: &'a Row, column: &str) -> Result<&'a str, Error> {
    match row.get(column) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(Error::Invalid),
    }
}

fn row_kind(row: &Row) -> Result<String, Error> {
    Ok(row_text(row, "kind")?.to_string())
}

fn row_audience(row: &Row) -> Result<Audience, Error> {
    Audience::parse(row_text(row, "audience")?)
}

fn row_role(row: &Row) -> Result<Role, Error> {
    Role::parse(row_text(row, "role")?)
}

fn row_identity(row: &Row) -> Result<&str, Error> {
    row_text(row, "identity")
}

fn row_resource(row: &Row) -> Result<&str, Error> {
    row_text(row, "resource")
}

/// Data interface for framework eligibility metadata.
pub fn data() -> snap_store::Data {
    snap_store::Data::new(&TABLES)
}
