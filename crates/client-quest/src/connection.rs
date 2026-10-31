// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Carries out the [`Conversation`]'s actions: owns the server connection and browses for servers while the server list shows.
use crate::{
    conversation::{Action, Conversation},
    identity::{Identity, Store},
    server_choice::{self, Choice, Source},
};
use client::{Client, ConnectionState, Event, Options, discovery};

/// Permission to receive multicast while browsing.
pub trait Multicast {
    fn set_held(&mut self, held: bool);
}

/// Desktop systems deliver multicast without a lock.
impl Multicast for () {
    fn set_held(&mut self, _held: bool) {}
}

pub struct Connection {
    identity: Identity,
    store: Store,
    multicast: Box<dyn Multicast>,
    client: Option<Client>,
    browser: Option<discovery::Browser>,
    servers: discovery::Servers,
    /// Whether the server list needs rebuilding.
    changed: bool,
    /// Browsing failed; it is tried again on request rather than every frame.
    failed: bool,
    remembered: Option<String>,
}

impl Connection {
    /// Connects straight away to a server given at startup; otherwise the conversation starts at the server list.
    pub fn new(
        identity: Identity,
        store: Store,
        multicast: Box<dyn Multicast>,
        conversation: &mut Conversation,
    ) -> Self {
        log::info!(
            "Participant {} as {:?}",
            identity.participant,
            identity.name
        );
        if let Some(address) = &identity.server {
            conversation.connect(Choice::new(address, address, Source::Override));
        }
        let remembered = store.server();
        Self {
            identity,
            store,
            multicast,
            client: None,
            browser: None,
            servers: discovery::Servers::new(),
            changed: true,
            failed: false,
            remembered,
        }
    }

    /// Run once per frame. Browsing, and the multicast lock it needs, stop while unfocused.
    pub fn poll(&mut self, focused: bool, conversation: &mut Conversation) {
        self.perform(conversation);
        self.browse(focused && conversation.is_browsing(), conversation);
        while let Some(event) = self.client.as_ref().and_then(Client::try_event) {
            if let Event::State(ConnectionState::Connected { .. }) = &event
                && let Some(instance) = conversation.server().and_then(|c| c.instance.as_deref())
            {
                self.store.remember_server(instance);
                self.remembered = Some(instance.to_owned());
            }
            conversation.apply(event);
        }
        self.perform(conversation);
    }

    fn perform(&mut self, conversation: &mut Conversation) {
        for action in conversation.take_actions() {
            match action {
                Action::Connect(choice) => {
                    self.disconnect();
                    let options = Options::new(
                        choice.address,
                        &self.identity.participant,
                        &self.identity.name,
                    );
                    match Client::connect(options) {
                        Ok(client) => self.client = Some(client),
                        Err(error) => {
                            conversation.fail(format!("Cannot start a connection: {error}"))
                        }
                    }
                }
                Action::Disconnect => self.disconnect(),
                Action::Rescan => {
                    self.browser = None;
                    self.servers = discovery::Servers::new();
                    self.changed = true;
                    self.failed = false;
                }
                Action::Send(message) => match &self.client {
                    Some(client) => {
                        if let Err(error) = client.send(message) {
                            conversation.fail(format!("Cannot send: {error}"));
                        }
                    }
                    None => conversation.fail("Not connected."),
                },
            }
        }
    }

    /// Close the connection without stalling the frame: closing waits briefly for the server.
    fn disconnect(&mut self) {
        if let Some(client) = self.client.take() {
            let closed = std::thread::Builder::new()
                .name("tomori-close".into())
                .spawn(move || client.close());
            if let Err(error) = closed {
                log::warn!("Closing the connection in the background failed: {error}");
            }
        }
    }

    fn browse(&mut self, browsing: bool, conversation: &mut Conversation) {
        if !browsing {
            if self.browser.take().is_some() {
                self.multicast.set_held(false);
            }
            return;
        }
        if self.browser.is_none() && !self.failed {
            self.multicast.set_held(true);
            match discovery::browse() {
                Ok(browser) => self.browser = Some(browser),
                Err(error) => {
                    self.multicast.set_held(false);
                    self.failed = true;
                    log::warn!("Cannot browse for servers: {error}");
                    conversation.fail(format!("Cannot search the network: {error}"));
                }
            }
        }
        if let Some(browser) = &self.browser {
            while let Ok(event) = browser.try_recv() {
                self.changed |= self.servers.apply(event);
            }
        }
        self.publish(conversation);
    }

    fn publish(&mut self, conversation: &mut Conversation) {
        if std::mem::take(&mut self.changed) {
            conversation.set_servers(server_choice::choices(
                self.identity.server.as_deref(),
                self.remembered.as_deref(),
                &self.servers,
            ));
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.browser = None;
        self.multicast.set_held(false);
        // Close in the foreground, so the server hears the goodbye before the process ends.
        if let Some(client) = self.client.take() {
            client.close();
        }
    }
}
