//! Host selections and optional guarantees for the shared contracts.
//! Atomic transactions and isolation are mandatory Store promises, not opt-outs.
//! Persistence is a setup promise: SQLite in memory is not crash-durable. These
//! declarations select tests; passing those tests, not declaring a flag, is proof.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guarantee {
    ProcessCrashDurability,
}

impl Guarantee {
    pub fn name(self) -> &'static str {
        match self {
            Self::ProcessCrashDurability => "process-crash durability",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Storage {
    Memory,
    SqliteMemory,
    SqliteFile,
}

impl Storage {
    pub const ALL: [Self; 3] = [Self::Memory, Self::SqliteMemory, Self::SqliteFile];

    pub fn id(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::SqliteMemory => "sqlite_memory",
            Self::SqliteFile => "sqlite_file",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "Memory",
            Self::SqliteMemory => "SQLite in memory",
            Self::SqliteFile => "SQLite file",
        }
    }

    pub fn provides(self, guarantee: Guarantee) -> bool {
        match guarantee {
            Guarantee::ProcessCrashDurability => self == Self::SqliteFile,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Carrier {
    Controlled,
    Tcp,
    WebSocket,
}

impl Carrier {
    pub const ALL: [Self; 3] = [Self::Controlled, Self::Tcp, Self::WebSocket];

    pub fn id(self) -> &'static str {
        match self {
            Self::Controlled => "controlled",
            Self::Tcp => "tcp",
            Self::WebSocket => "websocket",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Controlled => "controlled carrier",
            Self::Tcp => "TCP/TLS loopback",
            Self::WebSocket => "WebSocket loopback",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Host {
    pub storage: Storage,
    pub carrier: Carrier,
}

impl Host {
    /// Cheap representatives include controlled execution and both physical
    /// carriers. Full selection is Storage::ALL × Carrier::ALL, not extra cases.
    pub fn fast(self) -> bool {
        matches!(
            (self.storage, self.carrier),
            (Storage::Memory, Carrier::Controlled)
                | (Storage::SqliteMemory, Carrier::Tcp | Carrier::WebSocket)
        )
    }
}
