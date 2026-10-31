// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Resolving the server address given in [`crate::Options::address`].

use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Poll, Waker};

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

type Resolution = Result<Vec<SocketAddr>, Disconnect>;

#[derive(Default)]
struct Slot {
    resolution: Option<Resolution>,
    waker: Option<Waker>,
}

/// [`resolve`] on a helper thread, so that the connection thread can stop waiting, for example when the client closes. A host name lookup blocks in `getaddrinfo`, which cannot be interrupted; an abandoned lookup's thread ends by itself when the lookup returns. IP literals are resolved without a thread.
pub(crate) async fn resolve_in_background(address: &str) -> Resolution {
    resolve_with(address, resolve).await
}

async fn resolve_with(address: &str, resolver: fn(&str) -> Resolution) -> Resolution {
    if let Some(address) = literal(address) {
        return Ok(vec![address]);
    }
    let slot = Arc::new(Mutex::new(Slot::default()));
    let thread_slot = slot.clone();
    let thread_address = address.to_owned();
    std::thread::Builder::new()
        .name("tomori-resolve".into())
        .spawn(move || {
            let resolution = resolver(&thread_address);
            let mut slot = thread_slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.resolution = Some(resolution);
            let waker = slot.waker.take();
            drop(slot);
            if let Some(waker) = waker {
                waker.wake();
            }
        })
        .map_err(|error| Disconnect::Address(error.to_string()))?;
    std::future::poll_fn(|context| {
        let mut slot = slot.lock().unwrap_or_else(PoisonError::into_inner);
        match slot.resolution.take() {
            Some(resolution) => Poll::Ready(resolution),
            None => {
                slot.waker = Some(context.waker().clone());
                Poll::Pending
            }
        }
    })
    .await
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

    #[test]
    fn a_slow_lookup_can_be_abandoned() {
        use base_scheduler::Cancellation;
        use std::time::{Duration, Instant};

        fn slow(_: &str) -> Resolution {
            std::thread::sleep(Duration::from_secs(5));
            Ok(Vec::new())
        }

        let closing = Cancellation::new();
        let canceller = closing.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            canceller.cancel();
        });
        let started = Instant::now();
        let resolution = futures_lite::future::block_on(futures_lite::future::or(
            resolve_with("example.invalid", slow),
            async {
                closing.cancelled().await;
                Err(Disconnect::Closed)
            },
        ));
        assert_eq!(resolution, Err(Disconnect::Closed));
        assert!(started.elapsed() < Duration::from_secs(2));

        // A lookup that finishes wakes the waiting connection thread.
        let resolution = futures_lite::future::block_on(resolve_with("localhost", resolve));
        assert!(!resolution.unwrap().is_empty());
        // Literals need no lookup.
        let resolution = futures_lite::future::block_on(resolve_with("[::1]:9", slow));
        assert_eq!(resolution, Ok(vec![v6(9)]));
    }
}
