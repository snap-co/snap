//! Semantic ownership, independent of Cargo consumers and test techniques.
//! Unknown test targets retain their owner and name; they are never dropped just
//! because a display mapping has not been written for them.
use super::{Gate, Matrix};
use serde::Serialize;
use serde_json::Value;
use snap_platform_tests::configuration::{Carrier, Guarantee, Host, Storage};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub enum Layer {
    Interface,
    Core,
    Clients,
    Tooling,
}

impl Layer {
    pub const ALL: [Self; 4] = [Self::Interface, Self::Core, Self::Clients, Self::Tooling];
    pub fn name(self) -> &'static str {
        match self {
            Self::Interface => "Interface",
            Self::Core => "Core",
            Self::Clients => "Client adapters",
            Self::Tooling => "Tooling",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Contract {
    pub layer: Layer,
    pub module: String,
    pub contract: String,
    pub configuration: String,
    pub technique: &'static str,
    #[serde(skip)]
    pub storage: Option<Storage>,
    #[serde(skip)]
    pub host: Option<Host>,
    #[serde(serialize_with = "serialize_requirement")]
    pub requirement: Option<Guarantee>,
}

fn serialize_requirement<S: serde::Serializer>(
    requirement: &Option<Guarantee>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    requirement.map(Guarantee::name).serialize(serializer)
}

impl Contract {
    pub fn label(&self) -> String {
        format!(
            "{} / {} / {} / {}",
            self.layer.name(),
            self.module,
            self.contract,
            self.configuration
        )
    }

    pub fn selected(&self, gate: Gate, ignored: bool) -> bool {
        match gate {
            Gate::All => true,
            Gate::Interface => self.layer == Layer::Interface,
            Gate::Core => self.layer == Layer::Core,
            Gate::Clients => self.layer == Layer::Clients,
            Gate::Tooling => self.layer == Layer::Tooling,
            Gate::Test => self.technique != "properties" && !ignored,
            Gate::Properties => self.technique == "properties",
            Gate::Io => self.technique != "properties" && ignored,
            Gate::Check | Gate::Browser => false,
        }
    }

    pub fn omission(&self, matrix: Matrix) -> Option<String> {
        if let (Some(storage), Some(requirement)) = (self.storage, self.requirement)
            && !storage.provides(requirement)
        {
            return Some(format!("N/A · does not promise {}", requirement.name()));
        }
        if matches!(matrix, Matrix::Fast) {
            let selected = if let Some(host) = self.host {
                host.fast()
            } else {
                self.storage != Some(Storage::SqliteFile) || self.requirement.is_some()
            };
            if !selected {
                return Some("NOT SELECTED · use --matrix full".into());
            }
        }
        None
    }
}

pub fn describe(package: &Value, target: &Value, case: &str) -> Contract {
    let package_name = package["name"].as_str().unwrap_or("unknown");
    let target_name = target["name"].as_str().unwrap_or("unknown");
    let technique = if package_name == "snap-core-properties" {
        "properties"
    } else {
        "contracts"
    };
    let configurable = matches!(
        (package_name, target_name),
        ("snap-platform-tests", "native" | "matrix" | "dispatch")
            | ("snap-core-properties", "platform-dispatch")
            | ("snap-document", "server")
    );
    let storage = configurable
        .then(|| {
            Storage::ALL
                .into_iter()
                .find(|storage| case.split("::").any(|part| part == storage.id()))
        })
        .flatten();
    let carrier = Carrier::ALL
        .into_iter()
        .find(|carrier| case.split("::").any(|part| part == carrier.id()));
    let host = storage
        .zip(carrier)
        .map(|(storage, carrier)| Host { storage, carrier });
    let configuration = if let Some(host) = host {
        format!(
            "native / {} / {} / client + server SDK",
            host.storage.name(),
            host.carrier.name()
        )
    } else if let Some(storage) = storage {
        format!("native / {}", storage.name())
    } else if package_name == "snap-platform-tests" && target_name == "simulation" {
        "simulation / virtual time / client + server SDK".into()
    } else {
        "native".into()
    };
    let (layer, module, contract) = match package_name {
        "snap-platform-tests" => match target_name {
            "native" if storage.is_some() => {
                (Layer::Interface, "Store", "Transactions and residency")
            }
            "native" if case.starts_with("server_tcp::") => {
                (Layer::Interface, "Transport", "TCP/TLS · server contracts")
            }
            "native" if case.starts_with("server_websocket::") => (
                Layer::Interface,
                "Transport",
                "WebSocket · server contracts",
            ),
            "native" if case.starts_with("client_tcp::") => {
                (Layer::Interface, "Transport", "TCP/TLS · client contracts")
            }
            "matrix" => (
                Layer::Interface,
                "Transport + Store",
                "Client/server composition",
            ),
            "simulation" => (
                Layer::Interface,
                "Transport + Store",
                "Simulation fidelity and scheduling",
            ),
            "dispatch" => (
                Layer::Interface,
                "Transport + Store",
                "Server SDK admission and rollback",
            ),
            "cartridge_tcp" => (
                Layer::Interface,
                "Transport + Store",
                "TCP/TLS + SQLite · reopen and setup safety",
            ),
            "document_sync" => (Layer::Core, "Document", "Client/server synchronization"),
            "resources" => (Layer::Interface, "Store", "Resource lifecycle"),
            "host" => (Layer::Interface, "Transport", "Host execution lifecycle"),
            "host_tcp" => (Layer::Interface, "Transport", "TCP/TLS · host lifecycle"),
            _ => (Layer::Interface, "Transport", target_name),
        },
        "snap-core-properties" => match target_name {
            "document-sync" => (Layer::Core, "Document", "Client/server synchronization"),
            "identity-properties" => (Layer::Core, "Identity", "Sessions and issuance"),
            "store-properties" => (Layer::Interface, "Store", "Transactions and residency"),
            "platform-dispatch" => (
                Layer::Interface,
                "Transport + Store",
                "Server SDK admission and rollback",
            ),
            "transport-client" => (Layer::Interface, "Transport", "Client SDK"),
            "transport-lifecycle" | "execution-connections" => {
                (Layer::Interface, "Transport", "Connection lifetime")
            }
            "execution-properties" => (Layer::Interface, "Transport", "Server SDK execution"),
            _ => (Layer::Interface, "Transport", target_name),
        },
        "snap-document"
            if matches!(target_name, "server" | "client") && case.contains("manifest") =>
        {
            (Layer::Core, "Document", "Manifest")
        }
        "snap-document" => (
            Layer::Core,
            "Document",
            match target_name {
                "client" => "Client SDK",
                "server" => "Server SDK",
                "wire" => "Wire contracts",
                _ => target_name,
            },
        ),
        "snap-access" => (Layer::Core, "Access", "Authorization and transactions"),
        "snap-identity" => (
            Layer::Core,
            "Identity",
            match target_name {
                "oauth" => "OAuth",
                "operations" => "Server SDK operations",
                _ => "Credentials and sessions",
            },
        ),
        "snap-oidc" => (Layer::Core, "OIDC", "Provider contracts"),
        "snap-store" => (
            Layer::Interface,
            "Store",
            match target_name {
                "inbox" => "Ordered channels",
                "inbox_concurrency" => "Concurrent channels",
                "resident" => "Commit outcomes and residency",
                _ => target_name,
            },
        ),
        "snap-store-sqlite" => (
            Layer::Interface,
            "Store",
            match target_name {
                "recovery" => "Process-crash durability",
                "durable" => "SQLite representation",
                "migrations" => "Migrations",
                _ => target_name,
            },
        ),
        "snap-transport" => (
            Layer::Interface,
            "Transport",
            match target_name {
                "native_tls" => "TCP/TLS · certificate and handshake contracts",
                "native_binary" => "TCP · framing",
                "native_dispatch" => "Native dispatch lifecycle",
                "client" => "Client SDK",
                "execution" | "transactional" => "Server SDK execution",
                "binary" => "Wire framing",
                "connections" => "Connection lifetime",
                "inbox" => "Carrier queues",
                "operations" => "Operation registration",
                "snap_transport" => "Dispatch maintenance",
                _ => target_name,
            },
        ),
        "snap-crypto" => (
            Layer::Interface,
            "Crypto",
            match target_name {
                "native" => "Passwords and tokens",
                "token" => "Token verification",
                "passkey" => "WebAuthn verification",
                _ => target_name,
            },
        ),
        "snap-cli" => (
            Layer::Tooling,
            "CLI",
            match target_name {
                "application" => "Packaging and environment selection",
                "check" => "Static check selection",
                "check_contract" => "Declared suites and ownership",
                "dev" => "Development lifecycle",
                "migrate" => "Migration lifecycle",
                "secrets" => "Secret authoring and file permissions",
                "verify" => "Framework routing, caching and cancellation",
                "snap" => "Command defaults and readiness",
                _ => target_name,
            },
        ),
        "snap-config" => (Layer::Tooling, "Deployment configuration", target_name),
        "snap-http" => (Layer::Interface, "HTTP", target_name),
        "snap-react-bindings" => (Layer::Clients, "React", target_name),
        "snap-wasm-browser" => (Layer::Clients, "Browser/Wasm", target_name),
        _ => {
            let layer = match package["metadata"]["snap"]["role"].as_str() {
                Some("core") => Layer::Core,
                Some("interface" | "platform") => Layer::Interface,
                _ => Layer::Tooling,
            };
            (layer, package_name, target_name)
        }
    };
    let mut result = Contract {
        layer,
        module: module.into(),
        contract: contract.into(),
        configuration,
        technique,
        storage,
        host,
        requirement: None,
    };
    if package_name == "snap-store-sqlite" {
        result.storage = Some(Storage::SqliteFile);
        result.configuration = "native / SQLite file".into();
        if target_name == "recovery" {
            result.requirement = Some(Guarantee::ProcessCrashDurability);
        } else {
            // Backend-specific representation tests have no interchangeable
            // setup; fast mode must not discard them as optional matrix rows.
            result.storage = None;
        }
    }
    if package_name == "snap-document" && storage.is_none() {
        result.configuration = if case == "receipts_survive_restart_and_manifest_recovers" {
            "native / SQLite file / server SDK · orderly reopen"
        } else if target_name == "server" {
            "native / SQLite in memory / server SDK"
        } else if target_name == "wire" {
            "native / independent wire examples"
        } else {
            "native / controlled client SDK"
        }
        .into();
    } else if package_name == "snap-document" && target_name == "server" {
        result.configuration.push_str(" / server SDK");
    }
    if package_name == "snap-core-properties"
        && (target_name == "store-properties"
            || (target_name == "identity-properties"
                && case == "session_histories_match_authority_model"))
    {
        result.configuration = "native / SQLite in memory".into();
    }
    if package_name == "snap-core-properties"
        && target_name == "identity-properties"
        && case == "failed_issuance_never_returns_a_credential_or_partial_authority"
    {
        result.configuration = "native / controlled commit-fault backend".into();
    }
    if package_name == "snap-core-properties"
        && target_name == "store-properties"
        && case == "rejection_and_lost_commit_acknowledgement_never_serve_stale_memory"
    {
        result.configuration = "native / controlled commit-fault backend".into();
    }
    result
}
