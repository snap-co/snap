//! Version 1 mutation programs. All integers are little-endian. The envelope is
//! `SNAPMUT\0`, u16 version, SHA-256 of the serialized Catalog, u32 total byte
//! length and u32 instruction count. Each instruction is a u32-length frame.
//! Opcodes 1/2/3 are Insert/Update/Delete; operands are table, then row, or key
//! and changed fields, or key. Strings/bytes and collections have u32 lengths.
//! Values use tags 1/2/3 for UTF-8 text/i64/bytes. Rows are ordered field/value
//! pairs. Unknown versions, opcodes, types, duplicate fields and trailing bytes
//! are errors, never skipped. Programs are limited to 16 MiB.
//!
//! This is a data protocol, not Rust object serialization. Instructions contain
//! resolved values, no code, pointers or external effects. Schema binding is
//! exact, including declaration order. Schema upgrades require an explicit
//! checkpoint/migration policy; old programs are not translated implicitly.
use crate::{Catalog, Error, Row, Value, kind};
use alloc::{string::String, vec::Vec};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"SNAPMUT\0";
const HEADER: usize = 50;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Instruction {
    Insert {
        table: String,
        row: Row,
    },
    /// Assign only these fields. The row must exist; primary keys cannot change.
    Update {
        table: String,
        key: Vec<Value>,
        changes: Row,
    },
    Delete {
        table: String,
        key: Vec<Value>,
    },
}

impl Instruction {
    pub fn table(&self) -> &str {
        match self {
            Self::Insert { table, .. }
            | Self::Update { table, .. }
            | Self::Delete { table, .. } => table,
        }
    }

    pub(crate) fn validate(&self, catalog: &Catalog) -> Result<(), Error> {
        let schema = catalog.table(self.table())?;
        let key = match self {
            Self::Insert { row, .. } => return schema.validate_row(row),
            Self::Update { key, changes, .. } => {
                if changes.is_empty() {
                    return Err(Error::Invalid);
                }
                for (name, value) in changes {
                    if schema.primary.contains(name) || schema.column(name)?.kind != kind(value) {
                        return Err(Error::Invalid);
                    }
                }
                key
            }
            Self::Delete { key, .. } => key,
        };
        if key.len() != schema.primary.len()
            || schema.primary.iter().zip(key).any(|(name, value)| {
                schema.column(name).map(|column| column.kind) != Ok(kind(value))
            })
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

/// Validated, immutable binary transaction program. Debug deliberately omits
/// operands: programs can contain passwords, tokens and other private material.
#[derive(Clone, PartialEq, Eq)]
pub struct Program {
    bytes: Vec<u8>,
}

impl core::fmt::Debug for Program {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Program")
            .field("instructions", &self.len())
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

pub(crate) fn schema_id(catalog: &Catalog) -> Result<[u8; 32], Error> {
    let bytes = serde_json::to_vec(catalog).map_err(|_| Error::Invalid)?;
    Ok(Sha256::digest(bytes).into())
}

impl Program {
    /// Assemble resolved instructions for a checkpoint or authorized replica.
    /// This validates schema and operands, not execution authority or row state.
    pub fn from_instructions(
        catalog: &Catalog,
        instructions: impl IntoIterator<Item = Instruction>,
    ) -> Result<Self, Error> {
        catalog.validate()?;
        let mut program = Self::empty(schema_id(catalog)?);
        for instruction in instructions {
            instruction.validate(catalog)?;
            program.push(instruction)?;
        }
        Ok(program)
    }

    pub(crate) fn empty(schema: [u8; 32]) -> Self {
        let mut bytes = Vec::from(MAGIC.as_slice());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&schema);
        bytes.extend_from_slice(&(HEADER as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        Self { bytes }
    }

    /// Decode and validate an entire program before it can reach execution.
    /// This checks syntax and schema, not state, authority or concurrency.
    pub fn from_bytes(catalog: &Catalog, bytes: &[u8]) -> Result<Self, Error> {
        catalog.validate()?;
        if bytes.len() < HEADER
            || bytes.len() > MAX_BYTES
            || &bytes[..8] != MAGIC
            || bytes[8..10] != 1u16.to_le_bytes()
            || bytes[10..42] != schema_id(catalog)?
            || u32::from_le_bytes(bytes[42..46].try_into().map_err(|_| Error::Invalid)?) as usize
                != bytes.len()
        {
            return Err(Error::Invalid);
        }
        let count = u32::from_le_bytes(bytes[46..50].try_into().map_err(|_| Error::Invalid)?);
        let mut reader = Reader(&bytes[HEADER..]);
        for _ in 0..count {
            reader.instruction()?.validate(catalog)?;
        }
        if !reader.0.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(Self {
            bytes: bytes.into(),
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn validate_schema(&self, catalog: &Catalog) -> Result<(), Error> {
        if !self.matches(&schema_id(catalog)?) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub fn len(&self) -> usize {
        u32::from_le_bytes(self.bytes[46..50].try_into().expect("validated header")) as usize
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Owned decoded operands for adapters. Only validated bytes can inhabit a
    /// Program. No borrowed Rust layouts cross the persistence interface.
    pub fn instructions(&self) -> impl Iterator<Item = Instruction> + '_ {
        let mut reader = Reader(&self.bytes[HEADER..]);
        (0..self.len()).map(move |_| reader.instruction().expect("validated instruction"))
    }

    pub(crate) fn matches(&self, schema: &[u8; 32]) -> bool {
        self.bytes[10..42] == *schema
    }

    pub(crate) fn push(&mut self, instruction: Instruction) -> Result<Instruction, Error> {
        let mut frame = Vec::new();
        frame.push(match &instruction {
            Instruction::Insert { .. } => 1,
            Instruction::Update { .. } => 2,
            Instruction::Delete { .. } => 3,
        });
        string(&mut frame, instruction.table())?;
        match &instruction {
            Instruction::Insert { row: fields, .. } => row(&mut frame, fields)?,
            Instruction::Update {
                key: values,
                changes,
                ..
            } => {
                key(&mut frame, values)?;
                row(&mut frame, changes)?;
            }
            Instruction::Delete { key: values, .. } => key(&mut frame, values)?,
        }
        let size = self
            .bytes
            .len()
            .checked_add(4)
            .and_then(|n| n.checked_add(frame.len()))
            .ok_or(Error::Invalid)?;
        if size > MAX_BYTES {
            return Err(Error::Invalid);
        }
        let start = self.bytes.len();
        length(&mut self.bytes, frame.len())?;
        self.bytes.extend_from_slice(&frame);
        let count = u32::try_from(self.len() + 1).map_err(|_| Error::Invalid)?;
        self.bytes[42..46].copy_from_slice(&(size as u32).to_le_bytes());
        self.bytes[46..50].copy_from_slice(&count.to_le_bytes());
        // Local execution consumes the actual emitted bytes, exactly as host
        // persistence and replay do. This prototype decodes owned operands;
        // borrowed operands can remove those allocations later.
        drop(instruction);
        Reader(&self.bytes[start..]).instruction()
    }
}

fn length(out: &mut Vec<u8>, n: usize) -> Result<(), Error> {
    out.extend_from_slice(&u32::try_from(n).map_err(|_| Error::Invalid)?.to_le_bytes());
    Ok(())
}
fn blob(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    if bytes.len() > MAX_BYTES
        || out.len().saturating_add(4).saturating_add(bytes.len()) > MAX_BYTES
    {
        return Err(Error::Invalid);
    }
    length(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}
fn string(out: &mut Vec<u8>, text: &str) -> Result<(), Error> {
    blob(out, text.as_bytes())
}
fn value(out: &mut Vec<u8>, v: &Value) -> Result<(), Error> {
    match v {
        Value::Text(text) => {
            out.push(1);
            string(out, text)?;
        }
        Value::Integer(n) => {
            out.push(2);
            out.extend_from_slice(&n.to_le_bytes());
        }
        Value::Bytes(bytes) => {
            out.push(3);
            blob(out, bytes)?;
        }
    }
    if out.len() > MAX_BYTES {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn key(out: &mut Vec<u8>, values: &[Value]) -> Result<(), Error> {
    length(out, values.len())?;
    for v in values {
        value(out, v)?;
    }
    Ok(())
}
fn row(out: &mut Vec<u8>, fields: &Row) -> Result<(), Error> {
    length(out, fields.len())?;
    for (name, v) in fields {
        string(out, name)?;
        value(out, v)?;
    }
    Ok(())
}

struct Reader<'a>(&'a [u8]);
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.0.len() {
            return Err(Error::Invalid);
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn length(&mut self) -> Result<usize, Error> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| Error::Invalid)?) as usize)
    }
    fn blob(&mut self) -> Result<&'a [u8], Error> {
        let n = self.length()?;
        self.take(n)
    }
    fn string(&mut self) -> Result<String, Error> {
        Ok(core::str::from_utf8(self.blob()?)
            .map_err(|_| Error::Invalid)?
            .into())
    }
    fn value(&mut self) -> Result<Value, Error> {
        match self.byte()? {
            1 => Ok(Value::Text(self.string()?)),
            2 => Ok(Value::Integer(i64::from_le_bytes(
                self.take(8)?.try_into().map_err(|_| Error::Invalid)?,
            ))),
            3 => Ok(Value::Bytes(self.blob()?.into())),
            _ => Err(Error::Invalid),
        }
    }
    fn key(&mut self) -> Result<Vec<Value>, Error> {
        let count = self.length()?;
        let mut values = Vec::new();
        for _ in 0..count {
            values.push(self.value()?);
        }
        Ok(values)
    }
    fn row(&mut self) -> Result<Row, Error> {
        let count = self.length()?;
        let mut fields = Row::new();
        for _ in 0..count {
            let name = self.string()?;
            if fields.insert(name, self.value()?).is_some() {
                return Err(Error::Invalid);
            }
        }
        Ok(fields)
    }
    fn instruction(&mut self) -> Result<Instruction, Error> {
        let mut frame = Reader(self.blob()?);
        let opcode = frame.byte()?;
        let table = frame.string()?;
        let instruction = match opcode {
            1 => Instruction::Insert {
                table,
                row: frame.row()?,
            },
            2 => Instruction::Update {
                table,
                key: frame.key()?,
                changes: frame.row()?,
            },
            3 => Instruction::Delete {
                table,
                key: frame.key()?,
            },
            _ => return Err(Error::Invalid),
        };
        if !frame.0.is_empty() {
            return Err(Error::Invalid);
        }
        Ok(instruction)
    }
}
