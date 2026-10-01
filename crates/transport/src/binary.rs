//! Binary v1 carrier contract. Header integers use network byte order; payloads
//! are one CBOR value using the existing Transport serde envelopes. EOF between
//! frames detaches, while Close explicitly retires the logical lifetime.
pub use crate::carrier::AttachmentInfo;
use crate::{Command, Error, Response};
use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

pub const HEADER_LEN: usize = 12;
pub const CONNECT: u8 = 1;
pub const MESSAGE: u8 = 2;
/// Consecutive non-interleaved pieces of one MESSAGE. Payload begins with total
/// logical byte length and byte offset, both big-endian u32, then raw CBOR bytes.
pub const SEGMENT: u8 = 3;
pub const CONNECT_LIMIT: usize = 4096;
pub const MESSAGE_LIMIT: usize = 65536;
pub const LOGICAL_MESSAGE_LIMIT: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connect {
    bearer: String,
    client_id: String,
}
#[derive(Serialize, Deserialize)]
enum ConnectReply {
    Attached {
        resumed: bool,
        retention_ms: u64,
        lifetime: String,
    },
    Failed(Error),
}

pub fn limit(kind: u8) -> Result<usize, Error> {
    match kind {
        CONNECT => Ok(CONNECT_LIMIT),
        MESSAGE | SEGMENT => Ok(MESSAGE_LIMIT),
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
    let logical_limit = if kind == MESSAGE {
        LOGICAL_MESSAGE_LIMIT
    } else {
        limit(kind)?
    };
    if body.len() > logical_limit {
        return Err(Error::Capacity);
    }
    if kind == MESSAGE && body.len() > MESSAGE_LIMIT {
        let mut frames = Vec::new();
        for (index, chunk) in body.chunks(MESSAGE_LIMIT - 8).enumerate() {
            let offset = index * (MESSAGE_LIMIT - 8);
            let mut payload = Vec::with_capacity(8 + chunk.len());
            payload.extend_from_slice(&(body.len() as u32).to_be_bytes());
            payload.extend_from_slice(&(offset as u32).to_be_bytes());
            payload.extend_from_slice(chunk);
            frames.extend(frame(SEGMENT, &payload));
        }
        return Ok(frames);
    }
    Ok(frame(kind, &body))
}
fn frame(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(HEADER_LEN + body.len());
    frame.extend_from_slice(b"SNAP");
    frame.extend_from_slice(&[1, kind, 0, 0]);
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(body);
    frame
}
pub fn segment(payload: &[u8]) -> Result<(usize, usize, &[u8]), Error> {
    if payload.len() < 9 || payload.len() > MESSAGE_LIMIT {
        return Err(Error::Protocol);
    }
    let total = u32::from_be_bytes(payload[..4].try_into().map_err(|_| Error::Protocol)?) as usize;
    let offset =
        u32::from_be_bytes(payload[4..8].try_into().map_err(|_| Error::Protocol)?) as usize;
    let data = &payload[8..];
    if total <= MESSAGE_LIMIT
        || total > LOGICAL_MESSAGE_LIMIT
        || offset.checked_add(data.len()).is_none_or(|end| end > total)
    {
        return Err(Error::Capacity);
    }
    Ok((total, offset, data))
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
    if bytes.len()
        > if kind == MESSAGE {
            LOGICAL_MESSAGE_LIMIT
        } else {
            limit(kind)?
        }
    {
        return Err(Error::Capacity);
    }
    if kind == CONNECT {
        let Connect { bearer, client_id } = decode(bytes)?;
        Ok(Command::Connect { bearer, client_id })
    } else if kind == MESSAGE {
        let command = decode(bytes)?;
        if matches!(command, Command::Connect { .. }) {
            return Err(Error::Protocol);
        }
        Ok(command)
    } else {
        Err(Error::Protocol)
    }
}
pub fn response(
    response: &Response,
    connect_reply: bool,
    attachment: Option<&AttachmentInfo>,
) -> Result<Vec<u8>, Error> {
    if connect_reply {
        encode(
            CONNECT,
            &match response {
                Response::Attached { resumed } => {
                    let attachment = attachment.ok_or(Error::Protocol)?;
                    ConnectReply::Attached {
                        resumed: *resumed,
                        retention_ms: attachment.retention_ms,
                        lifetime: attachment.lifetime.clone(),
                    }
                }
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
pub fn read_response(kind: u8, bytes: &[u8]) -> Result<(Response, Option<AttachmentInfo>), Error> {
    if bytes.len()
        > if kind == MESSAGE {
            LOGICAL_MESSAGE_LIMIT
        } else {
            limit(kind)?
        }
    {
        return Err(Error::Capacity);
    }
    if kind == CONNECT {
        Ok(match decode(bytes)? {
            ConnectReply::Attached {
                resumed,
                retention_ms,
                lifetime,
            } => (
                Response::Attached { resumed },
                Some(AttachmentInfo {
                    retention_ms,
                    lifetime,
                }),
            ),
            ConnectReply::Failed(error) => (Response::Failed(error), None),
        })
    } else if kind == MESSAGE {
        let response = decode(bytes)?;
        if matches!(response, Response::Attached { .. }) {
            return Err(Error::Protocol);
        }
        Ok((response, None))
    } else {
        Err(Error::Protocol)
    }
}
