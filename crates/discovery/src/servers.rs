// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

use std::collections::BTreeMap;

use crate::{Event, Server};

/// The servers currently on the network, built by applying browse [`Event`]s, for showing a server list.
#[derive(Clone, Debug, Default)]
pub struct Servers {
    servers: BTreeMap<String, Server>,
}

impl Servers {
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply an event; returns whether the list changed.
    pub fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::Found(server) | Event::Incompatible(server) => {
                let previous = self.servers.insert(server.instance.clone(), server.clone());
                previous.as_ref() != Some(&server)
            }
            Event::Lost { instance } => self.servers.remove(&instance).is_some(),
        }
    }

    /// Compatible servers, ordered by display name, then instance.
    pub fn compatible(&self) -> Vec<&Server> {
        self.sorted(true)
    }

    /// Servers that cannot be joined because of their protocol version, ordered like [`Self::compatible`].
    pub fn incompatible(&self) -> Vec<&Server> {
        self.sorted(false)
    }

    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    fn sorted(&self, compatible: bool) -> Vec<&Server> {
        let mut servers: Vec<&Server> = self
            .servers
            .values()
            .filter(|server| server.is_compatible() == compatible)
            .collect();
        servers.sort_by(|a, b| (&a.name, &a.instance).cmp(&(&b.name, &b.instance)));
        servers
    }
}

#[cfg(test)]
mod tests {
    use protocol::PROTOCOL_VERSION;

    use super::*;

    fn server(instance: &str, name: &str, protocol: u32) -> Server {
        Server {
            instance: instance.to_owned(),
            name: name.to_owned(),
            host: "tomori.local.".to_owned(),
            addresses: Vec::new(),
            port: 8765,
            protocol: Some(protocol),
        }
    }

    #[test]
    fn keeps_compatible_and_incompatible_servers_apart() {
        let mut servers = Servers::new();
        assert!(servers.apply(Event::Found(server("b", "Beta", PROTOCOL_VERSION))));
        assert!(servers.apply(Event::Found(server("a", "Alpha", PROTOCOL_VERSION))));
        assert!(servers.apply(Event::Incompatible(server(
            "old",
            "Old",
            PROTOCOL_VERSION + 1
        ))));
        assert!(!servers.apply(Event::Found(server("a", "Alpha", PROTOCOL_VERSION))));

        let names: Vec<&str> = servers
            .compatible()
            .iter()
            .map(|server| server.name.as_str())
            .collect();
        assert_eq!(names, ["Alpha", "Beta"]);
        assert_eq!(servers.incompatible()[0].instance, "old");

        assert!(servers.apply(Event::Lost {
            instance: "a".to_owned()
        }));
        assert!(!servers.apply(Event::Lost {
            instance: "a".to_owned()
        }));
        assert_eq!(servers.compatible().len(), 1);
    }
}
