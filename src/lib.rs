//! The official [Databento](https://databento.com) client library.
//! It provides clients for fast, safe streaming of both real-time and historical market data through
//! similar interfaces.
//! The library is built on top of the tokio asynchronous runtime and
//! [Databento's efficient binary encoding](https://docs.databento.com/knowledge-base/new-users/dbn-encoding).
//!
//! You can find getting started tutorials, full API method documentation, examples
//! with output on the [Databento docs site](https://docs.databento.com/?historical=rust&live=rust).
//!
//! # Feature flags
//! By default both features are enabled.
//! - `historical`: enables the [historical client](HistoricalClient) for data older than 24 hours
//! - `live`: enables the [live client](LiveClient) for real-time and intraday
//!   historical data

// Experimental feature to allow docs.rs to display features
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(clippy::missing_errors_doc)]

pub mod error;
#[cfg(feature = "historical")]
pub mod historical;
#[cfg(feature = "live")]
pub mod live;

pub use error::{Error, Result};
#[cfg(feature = "historical")]
pub use historical::Client as HistoricalClient;
#[cfg(feature = "live")]
pub use live::Client as LiveClient;
// Re-export to keep versions synchronized
pub use dbn;

use std::fmt::{Display, Write};

use log::error;
#[cfg(feature = "historical")]
use serde::{Deserialize, Deserializer};

/// A set of symbols for a particular [`SType`](dbn::enums::SType).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Symbols {
    /// Sentinel value for all symbols in a dataset.
    All,
    /// A set of symbols identified by their instrument IDs.
    Ids(Vec<u32>),
    /// A set of symbols.
    Symbols(Vec<String>),
}

const ALL_SYMBOLS: &str = "ALL_SYMBOLS";
const API_KEY_LENGTH: usize = 32;

impl Symbols {
    /// Returns the string representation for sending to the API.
    pub fn to_api_string(&self) -> String {
        match self {
            Symbols::All => ALL_SYMBOLS.to_owned(),
            Symbols::Ids(ids) => ids.iter().fold(String::new(), |mut acc, s| {
                if acc.is_empty() {
                    s.to_string()
                } else {
                    write!(acc, ",{s}").unwrap();
                    acc
                }
            }),
            Symbols::Symbols(symbols) => symbols.join(","),
        }
    }

    #[cfg(feature = "live")]
    pub(crate) fn to_chunked_api_string(&self) -> Vec<String> {
        const CHUNK_SIZE: usize = 128;
        match self {
            Symbols::All => vec![ALL_SYMBOLS.to_owned()],
            Symbols::Ids(ids) => ids
                .chunks(CHUNK_SIZE)
                .map(|chunk| {
                    chunk.iter().fold(String::new(), |mut acc, s| {
                        if acc.is_empty() {
                            s.to_string()
                        } else {
                            write!(acc, ",{s}").unwrap();
                            acc
                        }
                    })
                })
                .collect(),
            Symbols::Symbols(symbols) => symbols
                .chunks(CHUNK_SIZE)
                .map(|chunk| chunk.join(","))
                .collect(),
        }
    }
}

impl Display for Symbols {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Symbols::All => f.write_str(ALL_SYMBOLS),
            Symbols::Ids(ids) => {
                for (i, id) in ids.iter().enumerate() {
                    if i == 0 {
                        write!(f, "{id}")?;
                    } else {
                        write!(f, ", {id}")?;
                    }
                }
                Ok(())
            }
            Symbols::Symbols(symbols) => {
                for (i, sym) in symbols.iter().enumerate() {
                    if i == 0 {
                        write!(f, "{sym}")?;
                    } else {
                        write!(f, ", {sym}")?;
                    }
                }
                Ok(())
            }
        }
    }
}

impl From<&str> for Symbols {
    fn from(value: &str) -> Self {
        Symbols::Symbols(vec![value.to_owned()])
    }
}

impl From<u32> for Symbols {
    fn from(value: u32) -> Self {
        Symbols::Ids(vec![value])
    }
}

impl From<Vec<u32>> for Symbols {
    fn from(value: Vec<u32>) -> Self {
        Symbols::Ids(value)
    }
}

impl From<String> for Symbols {
    fn from(value: String) -> Self {
        Symbols::Symbols(vec![value])
    }
}

impl From<Vec<String>> for Symbols {
    fn from(value: Vec<String>) -> Self {
        Symbols::Symbols(value)
    }
}

impl From<Vec<&str>> for Symbols {
    fn from(value: Vec<&str>) -> Self {
        Symbols::Symbols(value.into_iter().map(ToOwned::to_owned).collect())
    }
}

pub(crate) fn validate_key(key: String) -> crate::Result<String> {
    if key.len() != API_KEY_LENGTH {
        Err(Error::bad_arg("key", "expected to be 32-characters long"))
    } else if !key.is_ascii() {
        error!("API key '{key}' contains non-ASCII characters");
        Err(Error::bad_arg(
            "key",
            "expected to be composed of only ASCII characters",
        ))
    } else {
        Ok(key)
    }
}

pub(crate) fn key_from_env() -> crate::Result<String> {
    std::env::var("DATABENTO_API_KEY").map_err(|e| Error::bad_arg("key", format!("{e:?}")))
}

#[cfg(feature = "historical")]
impl<'de> Deserialize<'de> for Symbols {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Helper {
            Id(u32),
            Ids(Vec<u32>),
            Symbol(String),
            Symbols(Vec<String>),
        }
        let ir = Helper::deserialize(deserializer)?;
        Ok(match ir {
            Helper::Id(id) => Symbols::Ids(vec![id]),
            Helper::Ids(ids) => Symbols::Ids(ids),
            Helper::Symbol(symbol) if symbol == ALL_SYMBOLS => Symbols::All,
            Helper::Symbol(symbol) => Symbols::Symbols(vec![symbol]),
            Helper::Symbols(symbols) => Symbols::Symbols(symbols),
        })
    }
}

#[cfg(test)]
const TEST_DATA_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");
#[cfg(test)]
pub(crate) fn zst_test_data_path(schema: dbn::enums::Schema) -> String {
    format!("{TEST_DATA_PATH}/test_data.{}.dbn.zst", schema.as_str())
}
#[cfg(test)]
pub(crate) fn body_contains(
    key: impl Display,
    val: impl Display,
) -> wiremock::matchers::BodyContainsMatcher {
    wiremock::matchers::body_string_contains(&format!("{key}={val}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_symbols() {
        const JSON: &str = r#"["ALL_SYMBOLS", [1, 2, 3], ["ESZ3", "CLZ3"], "TSLA", 1001]"#;
        let symbol_res: Vec<Symbols> = serde_json::from_str(JSON).unwrap();
        assert_eq!(symbol_res.len(), 5);
        assert_eq!(symbol_res[0], Symbols::All);
        assert_eq!(symbol_res[1], Symbols::Ids(vec![1, 2, 3]));
        assert_eq!(
            symbol_res[2],
            Symbols::Symbols(vec!["ESZ3".to_owned(), "CLZ3".to_owned()])
        );
        assert_eq!(symbol_res[3], Symbols::Symbols(vec!["TSLA".to_owned()]));
        assert_eq!(symbol_res[4], Symbols::Ids(vec![1001]));
    }
}
