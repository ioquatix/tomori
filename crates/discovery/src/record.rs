// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! The TXT record and the translation of resolved services into [`Event`]s, kept free of sockets so that it can be unit-tested.

use std::net::{IpAddr, SocketAddr, SocketAddrV6};

use protocol::PROTOCOL_VERSION;

use crate::{NAME_KEY, PROTOCOL_KEY, SERVICE_TYPE};

/// The longest DNS label, which bounds the instance name.
pub(crate) const INSTANCE_LIMIT: usize = 63;

/// The longest TXT string is 255 bytes, including `name=`.
const NAME_LIMIT: usize = 255 - NAME_KEY.len() - 1;

/// A server found by browsing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    /// The DNS-SD instance name, unique on the network; it identifies the server in [`Event::Lost`].
    pub instance: String,
    /// The display name from the TXT record, or the instance name if the record has none.
    pub name: String,
    /// The multicast DNS host name, such as `tomori-studio.local.`.
    pub host: String,
    /// The server's addresses with its port, most preferred first: routable IPv4, routable IPv6, then link-local addresses. IPv6 link-local addresses carry the scope (interface index) they were found on.
    pub addresses: Vec<SocketAddr>,
    pub port: u16,
    /// The advertised protocol version, or `None` if the TXT record has no valid one.
    pub protocol: Option<u32>,
}

impl Server {
    /// The most preferred address as `host:port`, suitable for `client::Options::address`.
    pub fn address(&self) -> Option<String> {
        self.addresses.first().map(SocketAddr::to_string)
    }

    /// Whether this build of the client can talk to the server.
    pub fn is_compatible(&self) -> bool {
        self.protocol == Some(PROTOCOL_VERSION)
    }
}

/// A change in the set of servers on the network.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A compatible server appeared, or its addresses or name changed. Repeated for the same instance as records update; treat it as an insert or replace.
    Found(Server),
    /// A server appeared whose protocol version differs from [`PROTOCOL_VERSION`] or is missing. It must not be offered for connection, but may be shown as unavailable.
    Incompatible(Server),
    /// The server with this instance name stopped advertising or expired.
    Lost { instance: String },
}

/// The TXT properties a server advertises.
pub(crate) fn properties(name: &str, protocol: u32) -> Vec<(String, String)> {
    vec![
        (PROTOCOL_KEY.to_owned(), protocol.to_string()),
        (NAME_KEY.to_owned(), truncate(name, NAME_LIMIT).to_owned()),
    ]
}

/// `name` cut to at most `limit` bytes on a character boundary.
pub(crate) fn truncate(name: &str, limit: usize) -> &str {
    if name.len() <= limit {
        return name;
    }
    let mut end = limit;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

/// The instance name from a full service name such as `Studio\.lan._tomori._tcp.local.`, with DNS-SD escapes removed.
pub(crate) fn instance(fullname: &str) -> String {
    let escaped = fullname
        .strip_suffix(SERVICE_TYPE)
        .and_then(|name| name.strip_suffix('.'))
        .unwrap_or(fullname);
    let mut result = String::with_capacity(escaped.len());
    let mut characters = escaped.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(next) = characters.next() {
                result.push(next);
            }
        } else {
            result.push(character);
        }
    }
    result
}

/// Most preferred first; see [`Server::addresses`].
fn rank(address: &SocketAddr) -> u8 {
    match address.ip() {
        IpAddr::V4(ip) if ip.is_loopback() => 5,
        IpAddr::V4(ip) if ip.is_link_local() => 2,
        IpAddr::V4(_) => 0,
        IpAddr::V6(ip) if ip.is_loopback() => 5,
        IpAddr::V6(ip) if ip.is_unicast_link_local() => 3,
        IpAddr::V6(_) => 1,
    }
}

/// Socket addresses for `port`, ordered by preference. An IPv6 address is paired with the interface index it was found on, which link-local addresses need in order to connect.
pub(crate) fn addresses(
    found: impl IntoIterator<Item = (IpAddr, u32)>,
    port: u16,
) -> Vec<SocketAddr> {
    let mut addresses: Vec<SocketAddr> = found
        .into_iter()
        .map(|(ip, scope)| match ip {
            IpAddr::V4(ip) => SocketAddr::from((ip, port)),
            IpAddr::V6(ip) => {
                let scope = if ip.is_unicast_link_local() { scope } else { 0 };
                SocketAddr::V6(SocketAddrV6::new(ip, port, 0, scope))
            }
        })
        .collect();
    addresses.sort_by_key(|address| (rank(address), *address));
    addresses.dedup();
    addresses
}

/// The event for a resolved service, given a TXT lookup.
pub(crate) fn resolved<'a>(
    fullname: &str,
    host: &str,
    addresses: Vec<SocketAddr>,
    port: u16,
    property: impl Fn(&str) -> Option<&'a str>,
) -> Event {
    let instance = instance(fullname);
    let name = property(NAME_KEY)
        .filter(|name| !name.is_empty())
        .map_or_else(|| instance.clone(), str::to_owned);
    let protocol = property(PROTOCOL_KEY).and_then(|value| value.trim().parse().ok());
    let server = Server {
        instance,
        name,
        host: host.to_owned(),
        addresses,
        port,
        protocol,
    };
    if server.is_compatible() {
        Event::Found(server)
    } else {
        Event::Incompatible(server)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn lookup(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn event(txt: &HashMap<String, String>) -> Event {
        resolved(
            "Studio._tomori._tcp.local.",
            "tomori-studio.local.",
            vec![SocketAddr::from(([192, 168, 1, 10], 8765))],
            8765,
            |key| txt.get(key).map(String::as_str),
        )
    }

    #[test]
    fn encodes_the_protocol_version_and_name() {
        assert_eq!(
            properties("Studio", 7),
            vec![
                ("protocol".to_owned(), "7".to_owned()),
                ("name".to_owned(), "Studio".to_owned())
            ]
        );
    }

    #[test]
    fn truncates_long_names_on_a_character_boundary() {
        let name = "é".repeat(200);
        let encoded = &properties(&name, 1)[1].1;
        assert!(encoded.len() + "name=".len() <= 255);
        assert!(encoded.chars().all(|character| character == 'é'));
        assert_eq!(truncate("añb", 2), "a");
    }

    #[test]
    fn reports_a_matching_protocol_as_found() {
        let Event::Found(server) = event(&lookup(&[
            ("protocol", &PROTOCOL_VERSION.to_string()),
            ("name", "Samuel's Studio"),
        ])) else {
            panic!("expected Found");
        };
        assert_eq!(server.instance, "Studio");
        assert_eq!(server.name, "Samuel's Studio");
        assert_eq!(server.protocol, Some(PROTOCOL_VERSION));
        assert_eq!(server.address().as_deref(), Some("192.168.1.10:8765"));
    }

    #[test]
    fn reports_another_protocol_as_incompatible() {
        let other = (PROTOCOL_VERSION + 1).to_string();
        let Event::Incompatible(server) = event(&lookup(&[("protocol", &other)])) else {
            panic!("expected Incompatible");
        };
        assert_eq!(server.protocol, Some(PROTOCOL_VERSION + 1));
        assert!(!server.is_compatible());
    }

    #[test]
    fn reports_a_missing_or_invalid_protocol_as_incompatible() {
        for txt in [lookup(&[]), lookup(&[("protocol", "one")])] {
            let Event::Incompatible(server) = event(&txt) else {
                panic!("expected Incompatible");
            };
            assert_eq!(server.protocol, None);
        }
    }

    #[test]
    fn falls_back_to_the_instance_name() {
        let protocol = PROTOCOL_VERSION.to_string();
        for txt in [
            lookup(&[("protocol", &protocol)]),
            lookup(&[("protocol", &protocol), ("name", "")]),
        ] {
            let Event::Found(server) = event(&txt) else {
                panic!("expected Found");
            };
            assert_eq!(server.name, "Studio");
        }
    }

    #[test]
    fn unescapes_instance_names() {
        assert_eq!(
            instance(r"Studio\.lan\\b._tomori._tcp.local."),
            r"Studio.lan\b"
        );
        assert_eq!(instance("Samuel's Mac._tomori._tcp.local."), "Samuel's Mac");
    }

    #[test]
    fn orders_addresses_by_preference_with_link_local_scopes() {
        let link_local: Ipv6Addr = "fe80::1".parse().unwrap();
        let global: Ipv6Addr = "2001:db8::1".parse().unwrap();
        let ordered = addresses(
            [
                (IpAddr::V6(link_local), 4),
                (IpAddr::V4(Ipv4Addr::new(169, 254, 0, 2)), 4),
                (IpAddr::V4(Ipv4Addr::LOCALHOST), 1),
                (IpAddr::V6(global), 4),
                (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 4),
                (IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10)), 5),
            ],
            8765,
        );
        let printed: Vec<String> = ordered.iter().map(ToString::to_string).collect();
        assert_eq!(
            printed,
            [
                "192.168.1.10:8765",
                "[2001:db8::1]:8765",
                "169.254.0.2:8765",
                "[fe80::1%4]:8765",
                "127.0.0.1:8765",
            ]
        );
        // The client parses the printed form back, scope included.
        assert_eq!(
            "[fe80::1%4]:8765".parse::<SocketAddr>().unwrap(),
            ordered[3]
        );
    }
}
