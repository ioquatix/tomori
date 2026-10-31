// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

//! Application-owned selection; the concrete Sky renderer lives in a sub-crate.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Environment {
    /// Also the wire default for older headset builds which only drew the grid.
    #[default]
    Minimal,
    Sky,
}

impl Environment {
    pub fn initial() -> Self {
        match std::env::var("TOMORI_ENVIRONMENT").as_deref() {
            Ok("minimal") => Self::Minimal,
            Ok("sky") | Err(_) => Self::Sky,
            Ok(other) => {
                log::warn!("Unknown TOMORI_ENVIRONMENT {other:?}; using sky");
                Self::Sky
            }
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Minimal => Self::Sky,
            Self::Sky => Self::Minimal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_roundtrips_and_rejects_unknown_presets() {
        for value in [Environment::Minimal, Environment::Sky] {
            assert_eq!(
                serde_json::from_str::<Environment>(&serde_json::to_string(&value).unwrap())
                    .unwrap(),
                value
            );
            assert_eq!(value.next().next(), value);
        }
        assert!(serde_json::from_str::<Environment>("\"unknown\"").is_err());
        assert!(serde_json::from_str::<Environment>("null").is_err());
    }
}
