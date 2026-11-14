// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Admission at the door: the credentials on the HTTP request that opens a connection decide what it is admitted as, before it reaches the hub. See `design/security.md`.
use std::sync::Arc;

use tomori_security::{
    Authenticator, Chain, Credentials, Grant as Granted, Outcome, TokenStore, Verdict,
};

use crate::access::{Access, Grant};

/// The server's authentication mode.
pub enum Door {
    /// `--auth open`: everyone is an owner, and credentials are ignored.
    Open,
    /// Tokens the server issued admit their holders; others are visitors, if visitors are allowed.
    Tokens {
        chain: Chain,
        store: Arc<TokenStore>,
        visitors: bool,
    },
}

/// Revoke the token `id` from `store`, saying so if the store could not be written. Returns whether a token was revoked.
pub(crate) fn revoke(store: &TokenStore, id: &str) -> bool {
    store.revoke(id).unwrap_or_else(|error| {
        log::warn!("Could not revoke token {id}: {error}.");
        false
    })
}

/// The token store as an authenticator in the chain.
struct Tokens(Arc<TokenStore>);

impl Authenticator for Tokens {
    fn authenticate(&self, credentials: &Credentials) -> Verdict {
        self.0.authenticate(credentials)
    }
}

impl Door {
    /// Admit holders of the tokens in `store`, and, if `visitors`, anyone else to watch.
    pub fn tokens(store: Arc<TokenStore>, visitors: bool) -> Self {
        Self::Tokens {
            chain: Chain::new().with(Tokens(store.clone())),
            store,
            visitors,
        }
    }

    /// What `credentials` admit, or `None` if they are refused: credentials that no authenticator accepts, or none where visitors are not allowed, unless the request asks `to_pair`, which admits it only to pair.
    pub fn admit(&self, credentials: &Credentials, to_pair: bool) -> Option<Access> {
        let Self::Tokens {
            chain,
            store,
            visitors,
        } = self
        else {
            return Some(Access::open());
        };
        match chain.authenticate(credentials) {
            Outcome::Anonymous if *visitors => Some(Access::visitor()),
            Outcome::Anonymous => to_pair.then(Access::pairing),
            Outcome::Refused => None,
            Outcome::Authenticated(principal) => {
                // The store decides what the principal may do, and what they are called; a token revoked since it authenticated admits nothing.
                let entry = store.entry(&principal.id)?;
                let grant = match entry.grant {
                    Granted::Owner => Grant::Owner,
                    Granted::Collaborator { space } => Grant::Collaborator { space },
                };
                Some(Access {
                    principal: Some(principal.id),
                    name: Some(entry.name),
                    grant,
                })
            }
        }
    }

    /// The token store, unless everyone is admitted.
    pub fn store(&self) -> Option<&Arc<TokenStore>> {
        match self {
            Self::Open => None,
            Self::Tokens { store, .. } => Some(store),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (Arc<TokenStore>, std::path::PathBuf) {
        let directory =
            std::env::temp_dir().join(format!("tomori-door-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        (Arc::new(TokenStore::open(&directory).unwrap()), directory)
    }

    fn token(token: &str) -> Credentials {
        Credentials {
            token: Some(token.to_owned()),
        }
    }

    /// Authenticates everyone as the principal of a token that is no longer issued, as the store does for a token revoked after it authenticated.
    struct Revoked;

    impl Authenticator for Revoked {
        fn authenticate(&self, _: &Credentials) -> Verdict {
            Verdict::Authenticated(tomori_security::Principal {
                id: "token:revoked".to_owned(),
                name: "Revoked".to_owned(),
            })
        }
    }

    #[test]
    fn a_token_revoked_after_it_authenticated_admits_nothing() {
        let (store, directory) = store("revoked");
        let door = Door::Tokens {
            chain: Chain::new().with(Revoked),
            store,
            visitors: true,
        };
        assert_eq!(door.admit(&token("anything"), false), None);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_token_that_cannot_be_revoked_is_said_not_to_be() {
        crate::test_log::record();
        let (store, directory) = store("unrevoked");
        let (entry, _) = store.issue("Ada", Granted::Owner).unwrap();
        // The lock file each change opens cannot be opened where a directory has its name.
        std::fs::remove_file(directory.join("tokens.lock")).unwrap();
        std::fs::create_dir(directory.join("tokens.lock")).unwrap();
        assert!(!revoke(&store, &entry.id));
        assert_eq!(store.entries().len(), 1);
        let _ = std::fs::remove_dir_all(directory);
        assert!(!crate::test_log::logged("Could not revoke token").is_empty());
    }

    #[test]
    fn open_admits_everyone_as_an_owner() {
        assert_eq!(
            Door::Open.admit(&token("anything"), false),
            Some(Access::open())
        );
        assert!(Door::Open.store().is_none());
    }

    #[test]
    fn tokens_admit_their_grant_and_others_are_visitors_or_refused() {
        let (store, directory) = store("grants");
        let (owner, owner_token) = store.issue("Ada", Granted::Owner).unwrap();
        let (_, collaborator_token) = store
            .issue(
                "Bob",
                Granted::Collaborator {
                    space: "studio".into(),
                },
            )
            .unwrap();
        let door = Door::tokens(store.clone(), true);
        assert_eq!(
            door.admit(&token(&owner_token), false),
            Some(Access {
                principal: Some(owner.principal().id),
                name: Some(owner.name.clone()),
                grant: Grant::Owner,
            })
        );
        assert!(matches!(
            door.admit(&token(&collaborator_token), false),
            Some(Access { grant: Grant::Collaborator { space }, .. }) if space == "studio"
        ));
        assert_eq!(
            door.admit(&Credentials::default(), false),
            Some(Access::visitor())
        );
        assert_eq!(door.admit(&token("unknown"), false), None);
        assert_eq!(
            door.admit(&token(&tomori_security::generate_token()), false),
            None
        );
        assert!(door.store().is_some());

        let closed = Door::tokens(store, false);
        assert_eq!(closed.admit(&Credentials::default(), false), None);
        // Without visitors, a request to pair is admitted only to pair.
        assert_eq!(
            closed.admit(&Credentials::default(), true),
            Some(Access::pairing())
        );
        assert_eq!(
            door.admit(&Credentials::default(), true),
            Some(Access::visitor())
        );
        assert!(closed.admit(&token(&owner_token), false).is_some());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
