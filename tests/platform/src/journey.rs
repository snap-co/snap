//! Portable cartridge client. Hosts supply an attached Channel and drive its IO;
//! this journey knows neither sockets nor databases. Expectations are computed
//! from inputs, not from the server's responses.
use crate::{cartridge, dispatch::predict};
use alloc::{vec, vec::Vec};
use snap_transport::{
    Channel, Error, Operation, Outcome, Value,
    client::{Client, Pump},
    json,
};

#[derive(Debug)]
pub enum Failure {
    Transport(Error),
    Encoding,
    Observation {
        operation: &'static str,
        id: u64,
        expected: Pump,
        actual: Pump,
    },
}
impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Transport(error)
    }
}

/// Diagnostics only. Observers cannot supply responses or expected outcomes.
pub enum Observation<'a> {
    Sent {
        operation: &'static str,
        id: u64,
        input: &'a Value,
    },
    Received {
        operation: &'static str,
        observation: &'a Pump,
    },
}

async fn exchange<C: Channel, O: Operation>(
    client: &mut Client<C>,
    input: &O::Input,
    accepted: bool,
    outcome: Outcome,
    report: &mut impl FnMut(Observation<'_>),
) -> Result<(), Failure> {
    let input = serde_json::to_value(input).map_err(|_| Failure::Encoding)?;
    let id = client.begin(O::NAME, input.clone()).await?;
    report(Observation::Sent {
        operation: O::NAME,
        id,
        input: &input,
    });
    let mut expected: Vec<Pump> = vec![];
    if accepted {
        expected.push(Pump::Accepted { id });
    }
    expected.push(Pump::Completed { id, outcome });
    for expected in expected {
        let actual = client.pump().await?;
        report(Observation::Received {
            operation: O::NAME,
            observation: &actual,
        });
        if actual != expected {
            return Err(Failure::Observation {
                operation: O::NAME,
                id,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

/// Run the zero-baseline example through the real client SDK. This checks each
/// acceptance/completion and reads both records after every mutation or failure.
/// A native host must bound wall-clock execution; a simulator owns scheduling.
pub async fn run<C: Channel>(
    client: &mut Client<C>,
    mut report: impl FnMut(Observation<'_>),
) -> Result<i64, Failure> {
    let mut value = 0;
    exchange::<_, cartridge::Read>(client, &(), true, Ok(json!([value, value])), &mut report)
        .await?;
    for edit in cartridge::example() {
        let (accepted, outcome) = predict(&mut value, &edit);
        exchange::<_, cartridge::Change>(client, &edit, accepted, outcome, &mut report).await?;
        exchange::<_, cartridge::Read>(client, &(), true, Ok(json!([value, value])), &mut report)
            .await?;
    }
    Ok(value)
}
