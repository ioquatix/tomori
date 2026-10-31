// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Resolving the server address given in [`crate::Options::address`].

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};

use protocol::DEFAULT_PORT;

use crate::Disconnect;

/// Every address `address` names, in the resolver's order, for connecting to each in turn.
///
/// Accepts an IP literal with a port (`192.168.1.10:8765`, `[::1]:8765`), an IP literal alone (`192.168.1.10`, `::1`, `[::1]`), `host:port`, or a host alone. Addresses without a port use [`DEFAULT_PORT`].
pub(crate) fn resolve(address: &str) -> Result<Vec<SocketAddr>, Disconnect> {
    if let Some(address) = literal(address) {
        return Ok(vec![address]);
    }
    let resolved = match address.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| Disconnect::Address(format!("{address} has an invalid port")))?;
            (host, port).to_socket_addrs()
        }
        None => (address, DEFAULT_PORT).to_socket_addrs(),
    };
    let resolved: Vec<SocketAddr> = resolved
        .map_err(|error| Disconnect::Address(error.to_string()))?
        .collect();
    if resolved.is_empty() {
        return Err(Disconnect::Address(format!("{address} has no addresses")));
    }
    Ok(resolved)
}

/// An IP address literal, with or without a port, which needs no lookup.
pub(crate) fn literal(address: &str) -> Option<SocketAddr> {
    if let Ok(address) = address.parse::<SocketAddr>() {
        return Some(address);
    }
    let ip = address
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(address);
    ip.parse::<IpAddr>()
        .ok()
        .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv4Addr::LOCALHOST, port))
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::from((Ipv6Addr::LOCALHOST, port))
    }

    #[test]
    fn resolves_ip_literals_with_and_without_ports() {
        let cases = [
            ("127.0.0.1", v4(DEFAULT_PORT)),
            ("127.0.0.1:9", v4(9)),
            ("::1", v6(DEFAULT_PORT)),
            ("[::1]", v6(DEFAULT_PORT)),
            ("[::1]:9", v6(9)),
            (
                "fe80::1",
                SocketAddr::new("fe80::1".parse().unwrap(), DEFAULT_PORT),
            ),
        ];
        for (address, expected) in cases {
            assert_eq!(resolve(address).unwrap(), [expected], "{address}");
        }
    }

    #[test]
    fn resolves_host_names_to_every_address() {
        let addresses = resolve("localhost").unwrap();
        assert!(!addresses.is_empty());
        assert!(
            addresses
                .iter()
                .all(|address| address.port() == DEFAULT_PORT)
        );
        assert!(addresses.iter().all(|address| address.ip().is_loopback()));
        let addresses = resolve("localhost:9").unwrap();
        assert!(addresses.iter().all(|address| address.port() == 9));
    }

    #[test]
    fn rejects_invalid_addresses() {
        for address in ["invalid host name", "localhost:port", "[::1]:x", "::1]:9"] {
            assert!(
                matches!(resolve(address), Err(Disconnect::Address(_))),
                "{address}"
            );
        }
    }
}
