// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Advertise and browse in one process over real multicast DNS.

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, Instant};

use discovery::{Event, advertise, browse};
use protocol::PROTOCOL_VERSION;

/// Wait for the first event matching `predicate`, ignoring other servers on the network.
fn wait_for(
    browser: &discovery::Browser,
    timeout: Duration,
    predicate: impl Fn(&Event) -> bool,
) -> Option<Event> {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        match browser.recv_timeout(remaining) {
            Ok(event) if predicate(&event) => return Some(event),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

#[test]
#[ignore = "needs a network interface with working multicast, which CI containers often lack; run with --ignored"]
fn finds_an_advertised_server_and_loses_it_when_withdrawn() {
    let instance = format!("Round Trip {}", std::process::id());
    let browser = browse().expect("browse");
    let advertisement =
        advertise(&instance, 48765, IpAddr::V4(Ipv4Addr::UNSPECIFIED)).expect("advertise");

    let found = wait_for(
        &browser,
        Duration::from_secs(15),
        |event| matches!(event, Event::Found(server) if server.instance == instance),
    );
    let Some(Event::Found(server)) = found else {
        panic!("the advertised server was not found");
    };
    assert_eq!(server.name, instance);
    assert_eq!(server.port, 48765);
    assert_eq!(server.protocol, Some(PROTOCOL_VERSION));
    assert!(!server.addresses.is_empty());
    assert!(server.addresses.iter().all(|address| address.is_ipv4()));

    drop(advertisement);
    let lost = wait_for(
        &browser,
        Duration::from_secs(10),
        |event| matches!(event, Event::Lost { instance: lost } if *lost == instance),
    );
    assert!(lost.is_some(), "the withdrawn server was not lost");
}
