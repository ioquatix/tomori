// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Bonjour discovery of Tomori servers: DNS service discovery (RFC 6763) over multicast DNS (RFC 6762).
//!
//! A server [`advertise`]s an instance of the service type [`SERVICE_TYPE`] (`_tomori._tcp`) with two TXT keys:
//!
//! - `protocol=<u32>`: the [`protocol::PROTOCOL_VERSION`] the server speaks.
//! - `name=<display name>`: the name to show in a server list. The DNS-SD instance name normally equals it, but the instance may be renamed (`Name (2)`) to resolve a conflict on the network.
//!
//! A client [`browse`]s and receives [`Event`]s from a plain blocking channel; no async runtime is needed. Servers whose protocol version differs from this build's are reported as [`Event::Incompatible`], never as [`Event::Found`]. [`Servers`] folds events into a list.
//!
//! The implementation is [`mdns_sd`], a pure Rust responder and querier that runs its own thread and sockets. It answers on every interface, IPv4 and IPv6, and coexists with the system responder (mDNSResponder on macOS, Avahi on Linux), so `dns-sd -B _tomori._tcp` sees the server.
//!
//! See `design/server.md`.
//!
//! # Android and Meta Quest
//!
//! Android drops incoming multicast packets on Wi-Fi unless an application holds a `WifiManager.MulticastLock`, and Quest is Android. Without the lock, [`browse`] starts without error but never receives a response, so the server list stays empty. A native Quest client (`client-quest`) that uses this crate must:
//!
//! 1. Declare these permissions in the manifest. Both are "normal" permissions, granted at install time, with no runtime prompt:
//!
//!    ```toml
//!    [[package.metadata.android.uses_permission]]
//!    name = "android.permission.CHANGE_WIFI_MULTICAST_STATE"
//!
//!    [[package.metadata.android.uses_permission]]
//!    name = "android.permission.ACCESS_WIFI_STATE"
//!    ```
//!
//!    `INTERNET` is also needed and is already declared. `ACCESS_NETWORK_STATE` is not required.
//!
//! 2. Before calling [`browse`], acquire a multicast lock through JNI, from the activity's `JavaVM` and context (with `android-activity`, `AndroidApp::vm_as_ptr()` and `AndroidApp::activity_as_ptr()`):
//!
//!    ```text
//!    WifiManager wifi = (WifiManager) context.getApplicationContext().getSystemService("wifi");
//!    WifiManager.MulticastLock lock = wifi.createMulticastLock("tomori-discovery");
//!    lock.setReferenceCounted(false);
//!    lock.acquire();
//!    ```
//!
//!    Keep a global reference to `lock` while browsing, and call `lock.release()` after dropping the [`Browser`], at the latest when the activity pauses: holding the lock costs battery because the Wi-Fi chip wakes for every multicast packet. Acquire it again on resume if the server list is still shown.
//!
//! 3. Browse only while the server list is visible, and drop the [`Browser`] once a server is chosen; the connection itself is plain unicast TCP and does not need the lock.
//!
//! The alternative is the platform's `android.net.nsd.NsdManager` (`discoverServices("_tomori._tcp", PROTOCOL_DNS_SD, listener)`), which uses the system mDNS daemon and needs no multicast lock. It is callback-based, so a native client would need `NsdManager.DiscoveryListener` and `ResolveListener` implementations in Java or Kotlin (the `client-quest` crate's `cargo apk` build has no Java sources today), discovered services must each be resolved to obtain addresses and TXT records, and before API 34 `resolveService` handles only one request at a time and `NsdServiceInfo` carries a single address. It would also duplicate the protocol filtering done here. The multicast lock keeps discovery in this crate, identical on every platform, for a few JNI calls, so it is the recommended approach; `MulticastLock` in `crates/client-quest/src/android.rs` implements it.
//!
//! Advertising from Android is not needed: headsets are clients.

mod advertise;
mod browse;
mod record;
mod servers;

use std::fmt;

pub use advertise::{Advertisement, advertise};
pub use browse::{Browser, browse};
pub use record::{Event, Server};
pub use servers::Servers;
pub use std::sync::mpsc::{RecvTimeoutError, TryRecvError};

/// The DNS-SD service type, with the `local.` domain, as multicast DNS names it.
pub const SERVICE_TYPE: &str = "_tomori._tcp.local.";

/// The TXT key holding the protocol version as a decimal `u32`.
pub const PROTOCOL_KEY: &str = "protocol";

/// The TXT key holding the display name.
pub const NAME_KEY: &str = "name";

/// A failure to start advertising or browsing, such as no usable network interface or socket.
#[derive(Debug)]
pub struct Error(mdns_sd::Error);

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "multicast DNS: {}", self.0)
    }
}

impl std::error::Error for Error {}

impl From<mdns_sd::Error> for Error {
    fn from(error: mdns_sd::Error) -> Self {
        Self(error)
    }
}
