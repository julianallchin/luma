//! OSC over UDP: each numeric argument of each message is one input.

use std::net::SocketAddr;

use rosc::{OscPacket, OscType};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use super::{Address, Event};

/// A bound socket. Dropping it stops listening.
pub(super) struct Listener {
    task: JoinHandle<()>,
    pub(super) address: SocketAddr,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// An argument as a control value: floats as sent, clamped to 0–1; ints as
/// MIDI-style 0–127; booleans as 0 or 1.
fn value(arg: &OscType) -> Option<f64> {
    let value = match arg {
        OscType::Float(value) => f64::from(*value),
        OscType::Double(value) => *value,
        OscType::Int(value) => f64::from(*value) / 127.0,
        OscType::Long(value) => *value as f64 / 127.0,
        OscType::Bool(on) => f64::from(u8::from(*on)),
        _ => return None,
    };
    value.is_finite().then(|| value.clamp(0.0, 1.0))
}

pub(super) fn events(packet: OscPacket, out: &mut Vec<Event>) {
    match packet {
        OscPacket::Message(message) => {
            for (arg, value) in message
                .args
                .iter()
                .enumerate()
                .filter_map(|(i, a)| Some((i, value(a)?)))
            {
                let Ok(arg) = u8::try_from(arg) else {
                    break;
                };
                out.push(Event {
                    address: Address::Osc {
                        address: message.addr.clone(),
                        arg,
                    },
                    value,
                });
            }
        }
        OscPacket::Bundle(bundle) => {
            for packet in bundle.content {
                events(packet, out);
            }
        }
    }
}

pub(super) async fn listen(
    address: &str,
    sink: impl Fn(Event) + Send + 'static,
) -> Result<Listener, String> {
    let socket = UdpSocket::bind(address)
        .await
        .map_err(|error| format!("Could not hear OSC on {address}: {error}"))?;
    let address = socket.local_addr().map_err(|error| error.to_string())?;
    let task = tokio::spawn(async move {
        let mut buffer = vec![0u8; rosc::decoder::MTU];
        let mut heard = Vec::new();
        loop {
            let Ok((len, _)) = socket.recv_from(&mut buffer).await else {
                continue;
            };
            let Ok((_, packet)) = rosc::decoder::decode_udp(&buffer[..len]) else {
                continue;
            };
            events(packet, &mut heard);
            for event in heard.drain(..) {
                sink(event);
            }
        }
    });
    Ok(Listener { task, address })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rosc::OscMessage;

    #[test]
    fn each_number_of_a_message_is_its_own_input() {
        let mut out = Vec::new();
        events(
            OscPacket::Message(OscMessage {
                addr: "/glove/tilt".into(),
                args: vec![
                    OscType::Float(0.25),
                    OscType::String("x".into()),
                    OscType::Float(2.0),
                ],
            }),
            &mut out,
        );
        let args: Vec<_> = out
            .iter()
            .map(|e| match &e.address {
                Address::Osc { arg, .. } => (*arg, e.value),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            args,
            vec![(0, 0.25), (2, 1.0)],
            "text is skipped and values clamp"
        );
    }
}
