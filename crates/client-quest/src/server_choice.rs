// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The servers offered for connection, in order of preference.
use discovery::Servers;
use protocol::PROTOCOL_VERSION;

/// A server on the PC reached over USB, with `adb reverse tcp:8765 tcp:8765`.
pub const USB: &str = "127.0.0.1:8765";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// Given when the app started, with `--server`, `TOMORI_SERVER` or the `tomori.server` intent extra.
    Override,
    /// Found by browsing, and the server this device last connected to.
    Remembered,
    /// Found by browsing.
    Discovered,
    /// The USB loopback address, always offered.
    Usb,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub label: String,
    /// `host:port`, or a host alone for the default port.
    pub address: String,
    /// The DNS-SD instance name of a discovered server, which is what is remembered.
    pub instance: Option<String>,
    pub source: Source,
    /// Why the server cannot be joined, for listing it greyed.
    pub unavailable: Option<String>,
}

impl Choice {
    pub fn new(label: impl Into<String>, address: impl Into<String>, source: Source) -> Self {
        Self {
            label: label.into(),
            address: address.into(),
            instance: None,
            source,
            unavailable: None,
        }
    }

    pub fn is_available(&self) -> bool {
        self.unavailable.is_none()
    }
}

/// Every server to list, most preferred first: an explicit override, the remembered server, the other compatible servers found by browsing, then the USB loopback address. Servers that cannot be joined follow, with the reason.
pub fn choices(
    override_address: Option<&str>,
    remembered: Option<&str>,
    servers: &Servers,
) -> Vec<Choice> {
    let mut choices = Vec::new();
    if let Some(address) = override_address {
        choices.push(Choice::new(address, address, Source::Override));
    }
    let mut unavailable = Vec::new();
    let mut discovered = Vec::new();
    for server in servers.compatible() {
        let remembered = remembered == Some(server.instance.as_str());
        let mut choice = Choice::new(
            &server.name,
            server.address().unwrap_or_default(),
            if remembered {
                Source::Remembered
            } else {
                Source::Discovered
            },
        );
        choice.instance = Some(server.instance.clone());
        if choice.address.is_empty() {
            choice.unavailable = Some("no address".into());
            unavailable.push(choice);
        } else {
            discovered.push(choice);
        }
    }
    // Stable, so the others keep the discovery crate's order.
    discovered.sort_by_key(|choice| choice.source != Source::Remembered);
    choices.extend(discovered);
    choices.push(Choice::new("USB (adb reverse)", USB, Source::Usb));
    // The same address under two names would only confuse.
    let mut seen = Vec::new();
    choices.retain(|choice| {
        let duplicate = seen.contains(&choice.address);
        seen.push(choice.address.clone());
        !duplicate
    });
    for server in servers.incompatible() {
        let mut choice = Choice::new(
            &server.name,
            server.address().unwrap_or_default(),
            Source::Discovered,
        );
        choice.instance = Some(server.instance.clone());
        choice.unavailable = Some(match server.protocol {
            Some(version) => format!("protocol {version}, needs {PROTOCOL_VERSION}"),
            None => "unknown protocol".into(),
        });
        unavailable.push(choice);
    }
    choices.extend(unavailable);
    choices
}

#[cfg(test)]
mod tests {
    use super::*;
    use discovery::{Event, Server};

    fn server(instance: &str, name: &str, address: &str, protocol: Option<u32>) -> Server {
        Server {
            instance: instance.into(),
            name: name.into(),
            host: format!("{instance}.local."),
            addresses: address.parse().into_iter().collect(),
            port: 8765,
            protocol,
        }
    }

    fn servers(list: &[Server]) -> Servers {
        let mut servers = Servers::new();
        for server in list {
            servers.apply(if server.is_compatible() {
                Event::Found(server.clone())
            } else {
                Event::Incompatible(server.clone())
            });
        }
        servers
    }

    fn summary(choices: &[Choice]) -> Vec<(&str, Source, bool)> {
        choices
            .iter()
            .map(|choice| {
                (
                    choice.address.as_str(),
                    choice.source,
                    choice.is_available(),
                )
            })
            .collect()
    }

    #[test]
    fn usb_is_offered_when_nothing_is_found() {
        let choices = choices(None, None, &Servers::new());
        assert_eq!(summary(&choices), [(USB, Source::Usb, true)]);
        assert_eq!(
            USB.rsplit_once(':').unwrap().1,
            protocol::DEFAULT_PORT.to_string()
        );
    }

    #[test]
    fn orders_override_remembered_discovered_then_usb() {
        let found = servers(&[
            server("alpha", "Alpha", "10.0.0.1:8765", Some(PROTOCOL_VERSION)),
            server("beta", "Beta", "10.0.0.2:8765", Some(PROTOCOL_VERSION)),
        ]);
        let choices = choices(Some("studio.local:9000"), Some("beta"), &found);
        assert_eq!(
            summary(&choices),
            [
                ("studio.local:9000", Source::Override, true),
                ("10.0.0.2:8765", Source::Remembered, true),
                ("10.0.0.1:8765", Source::Discovered, true),
                (USB, Source::Usb, true),
            ]
        );
        assert_eq!(choices[1].label, "Beta");
        assert_eq!(choices[1].instance.as_deref(), Some("beta"));
    }

    #[test]
    fn a_remembered_server_that_is_gone_is_not_listed() {
        let found = servers(&[server(
            "alpha",
            "Alpha",
            "10.0.0.1:8765",
            Some(PROTOCOL_VERSION),
        )]);
        let choices = choices(None, Some("gone"), &found);
        assert_eq!(
            summary(&choices),
            [
                ("10.0.0.1:8765", Source::Discovered, true),
                (USB, Source::Usb, true),
            ]
        );
    }

    #[test]
    fn lists_incompatible_servers_last_with_their_version() {
        let found = servers(&[
            server("old", "Old", "10.0.0.3:8765", Some(PROTOCOL_VERSION + 1)),
            server("odd", "Odd", "10.0.0.4:8765", None),
            server("alpha", "Alpha", "10.0.0.1:8765", Some(PROTOCOL_VERSION)),
        ]);
        let choices = choices(None, Some("old"), &found);
        assert_eq!(
            summary(&choices),
            [
                ("10.0.0.1:8765", Source::Discovered, true),
                (USB, Source::Usb, true),
                ("10.0.0.4:8765", Source::Discovered, false),
                ("10.0.0.3:8765", Source::Discovered, false),
            ]
        );
        assert_eq!(choices[2].unavailable.as_deref(), Some("unknown protocol"));
        assert_eq!(
            choices[3].unavailable,
            Some(format!(
                "protocol {}, needs {PROTOCOL_VERSION}",
                PROTOCOL_VERSION + 1
            ))
        );
    }

    #[test]
    fn an_override_hides_the_same_address_elsewhere() {
        let found = servers(&[server(
            "alpha",
            "Alpha",
            "10.0.0.1:8765",
            Some(PROTOCOL_VERSION),
        )]);
        let choices = choices(Some(USB), None, &found);
        assert_eq!(
            summary(&choices),
            [
                (USB, Source::Override, true),
                ("10.0.0.1:8765", Source::Discovered, true),
            ]
        );
        let listed = super::choices(Some("10.0.0.1:8765"), None, &found);
        assert_eq!(
            summary(&listed),
            [
                ("10.0.0.1:8765", Source::Override, true),
                (USB, Source::Usb, true),
            ]
        );
    }

    #[test]
    fn a_compatible_server_without_an_address_is_unavailable() {
        let mut unreachable = server("alpha", "Alpha", "", Some(PROTOCOL_VERSION));
        unreachable.addresses.clear();
        let choices = choices(None, None, &servers(&[unreachable]));
        assert_eq!(
            summary(&choices),
            [(USB, Source::Usb, true), ("", Source::Discovered, false)]
        );
        assert_eq!(choices[1].unavailable.as_deref(), Some("no address"));
    }
}
