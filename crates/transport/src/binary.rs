//! Binary v1 carrier contract. Header integers use network byte order; payloads
//! are one CBOR value using the existing Transport serde envelopes. EOF between
//! frames detaches, while Close explicitly retires the logical lifetime.
use crate::{Command, Error, Response};
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const HEADER_LEN: usize = 12;
pub const CONNECT: u8 = 1;
pub const MESSAGE: u8 = 2;
pub const CONNECT_LIMIT: usize = 4096;
pub const MESSAGE_LIMIT: usize = 65536;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connect {
    bearer: String,
    client_id: String,
}
#[derive(Serialize, Deserialize)]
enum ConnectReply {
    Attached { resumed: bool, retention_ms: u64 },
    Failed(Error),
}

pub fn limit(kind: u8) -> Result<usize, Error> {
    match kind {
        CONNECT => Ok(CONNECT_LIMIT),
        MESSAGE => Ok(MESSAGE_LIMIT),
        _ => Err(Error::Protocol),
    }
}

pub fn header(bytes: &[u8; HEADER_LEN]) -> Result<(u8, usize), Error> {
    if &bytes[..4] != b"SNAP" || bytes[4] != 1 || bytes[6..8] != [0, 0] {
        return Err(Error::Protocol);
    }
    let kind = bytes[5];
    let size = u32::from_be_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    if size > limit(kind)? {
        return Err(Error::Capacity);
    }
    Ok((kind, size))
}
fn decode<T: DeserializeOwned>(mut bytes: &[u8]) -> Result<T, Error> {
    let value = ciborium::de::from_reader_with_recursion_limit(&mut bytes, 32)
        .map_err(|_| Error::Protocol)?;
    if !bytes.is_empty() {
        return Err(Error::Protocol);
    }
    Ok(value)
}
fn encode<T: Serialize>(kind: u8, value: &T) -> Result<Vec<u8>, Error> {
    let mut body = Vec::new();
    ciborium::ser::into_writer(value, &mut body).map_err(|_| Error::Protocol)?;
    if body.len() > limit(kind)? {
        return Err(Error::Capacity);
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
    frame.extend_from_slice(b"SNAP");
    frame.extend_from_slice(&[1, kind, 0, 0]);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}
pub fn command(command: &Command) -> Result<Vec<u8>, Error> {
    match command {
        Command::Connect { bearer, client_id } => encode(
            CONNECT,
            &Connect {
                bearer: bearer.clone(),
                client_id: client_id.clone(),
            },
        ),
        command => encode(MESSAGE, command),
    }
}
pub fn read_command(kind: u8, bytes: &[u8]) -> Result<Command, Error> {
    if bytes.len() > limit(kind)? {
        return Err(Error::Capacity);
    }
    if kind == CONNECT {
        let Connect { bearer, client_id } = decode(bytes)?;
        Ok(Command::Connect { bearer, client_id })
    } else {
        let command = decode(bytes)?;
        if matches!(command, Command::Connect { .. }) {
            return Err(Error::Protocol);
        }
        Ok(command)
    }
}
pub fn response(
    response: &Response,
    connect_reply: bool,
    retention_ms: u64,
) -> Result<Vec<u8>, Error> {
    if connect_reply {
        encode(
            CONNECT,
            &match response {
                Response::Attached { resumed } => ConnectReply::Attached {
                    resumed: *resumed,
                    retention_ms,
                },
                Response::Failed(error) => ConnectReply::Failed(error.clone()),
                _ => return Err(Error::Protocol),
            },
        )
    } else {
        if matches!(response, Response::Attached { .. }) {
            return Err(Error::Protocol);
        }
        encode(MESSAGE, response)
    }
}
pub fn read_response(kind: u8, bytes: &[u8]) -> Result<(Response, Option<u64>), Error> {
    if bytes.len() > limit(kind)? {
        return Err(Error::Capacity);
    }
    if kind == CONNECT {
        Ok(match decode(bytes)? {
            ConnectReply::Attached {
                resumed,
                retention_ms,
            } => (Response::Attached { resumed }, Some(retention_ms)),
            ConnectReply::Failed(error) => (Response::Failed(error), None),
        })
    } else {
        let response = decode(bytes)?;
        if matches!(response, Response::Attached { .. }) {
            return Err(Error::Protocol);
        }
        Ok((response, None))
    }
}
