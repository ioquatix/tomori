// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::time::{Duration, Instant};

use mdns_sd::{Receiver, ResolvedService, ScopedIp, ServiceDaemon, ServiceEvent};

use crate::record::{addresses, instance, resolved};
use crate::{Error, Event, RecvTimeoutError, SERVICE_TYPE, TryRecvError};

/// How long dropping a [`Browser`] waits for the querier thread to stop.
const STOP_TIMEOUT: Duration = Duration::from_secs(1);

/// Browses for Tomori servers until dropped. Events are received with blocking calls from any thread, or by iterating, which ends only when the querier stops.
pub struct Browser {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Browser").finish_non_exhaustive()
    }
}

/// Start browsing for Tomori servers on every interface, IPv4 and IPv6.
///
/// On Android, the application must hold a Wi-Fi multicast lock first; see the crate documentation.
pub fn browse() -> Result<Browser, Error> {
    let daemon = ServiceDaemon::new()?;
    match daemon.browse(SERVICE_TYPE) {
        Ok(events) => Ok(Browser { daemon, events }),
        Err(error) => {
            let _ = daemon.shutdown();
            Err(error.into())
        }
    }
}

impl Browser {
    /// Wait for the next event; `None` once the querier has stopped.
    pub fn recv(&self) -> Option<Event> {
        loop {
            if let Some(event) = translate(self.events.recv().ok()?) {
                return Some(event);
            }
        }
    }

    /// Wait up to `timeout` for the next event.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    if let Some(event) = translate(event) {
                        return Ok(event);
                    }
                }
                Err(mdns_sd::RecvTimeoutError::Timeout) => return Err(RecvTimeoutError::Timeout),
                Err(mdns_sd::RecvTimeoutError::Disconnected) => {
                    return Err(RecvTimeoutError::Disconnected);
                }
            }
        }
    }

    /// The next event if one is ready.
    pub fn try_recv(&self) -> Result<Event, TryRecvError> {
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    if let Some(event) = translate(event) {
                        return Ok(event);
                    }
                }
                Err(mdns_sd::TryRecvError::Empty) => return Err(TryRecvError::Empty),
                Err(mdns_sd::TryRecvError::Disconnected) => {
                    return Err(TryRecvError::Disconnected);
                }
            }
        }
    }
}

impl Iterator for &Browser {
    type Item = Event;

    fn next(&mut self) -> Option<Event> {
        self.recv()
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        if let Ok(status) = self.daemon.shutdown() {
            let _ = status.recv_timeout(STOP_TIMEOUT);
        }
    }
}

fn translate(event: ServiceEvent) -> Option<Event> {
    match event {
        ServiceEvent::ServiceResolved(service) => Some(found(&service)),
        ServiceEvent::ServiceRemoved(_, fullname) => Some(Event::Lost {
            instance: instance(&fullname),
        }),
        _ => None,
    }
}

fn found(service: &ResolvedService) -> Event {
    let scoped = service.addresses.iter().map(|address| {
        let scope = match address {
            ScopedIp::V6(address) => address.scope_id().index,
            _ => 0,
        };
        (address.to_ip_addr(), scope)
    });
    resolved(
        &service.fullname,
        &service.host,
        addresses(scoped, service.port),
        service.port,
        |key| service.txt_properties.get_property_val_str(key),
    )
}
