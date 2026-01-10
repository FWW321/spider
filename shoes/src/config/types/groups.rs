//! Configuration group types (top-level Config, groups, and NamedPem).

use serde::{Deserialize, Serialize};
use crate::utils::option::OneOrSome;
use super::client::ClientConfig;
use super::rules::RuleConfig;
use super::selection::ConfigSelection;
use super::server::ServerConfig;

/// A named group of client proxies.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfigGroup {
    pub client_group: String,
    #[serde(alias = "client_proxy")]
    pub client_proxies: OneOrSome<ConfigSelection<ClientConfig>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfigGroup {
    pub rule_group: String,
    #[serde(alias = "rule")]
    pub rules: OneOrSome<RuleConfig>,
}

#[derive(Debug, Clone)]
pub struct NamedPem {
    pub pem: String,
    pub source: PemSource,
}

#[derive(Debug, Clone)]
pub enum PemSource {
    Path(String),
    Data(String),
}

impl<'de> Deserialize<'de> for NamedPem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        use serde::de::{Error, MapAccess, Visitor};
        use std::fmt;

        struct NamedPemVisitor;

        impl<'de> Visitor<'de> for NamedPemVisitor {
            type Value = NamedPem;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a NamedPem with 'pem' and either 'path' or 'data' fields")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut pem_name: Option<String> = None;
                let mut path: Option<String> = None;
                let mut data: Option<String> = None;

                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "pem" => pem_name = Some(map.next_value()?),
                        "path" => path = Some(map.next_value()?),
                        "data" => data = Some(map.next_value()?),
                        _ => { let _: serde::de::IgnoredAny = map.next_value()?; }
                    }
                }

                let pem_name = pem_name.ok_or_else(|| Error::missing_field("pem"))?;
                let source = match (path, data) {
                    (Some(p), None) => PemSource::Path(p),
                    (None, Some(d)) => PemSource::Data(d),
                    _ => return Err(Error::custom("NamedPem must have exactly one of 'path' or 'data' field")),
                };

                Ok(NamedPem { pem: pem_name, source })
            }
        }

        deserializer.deserialize_map(NamedPemVisitor)
    }
}

impl Serialize for NamedPem {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: serde::ser::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("pem", &self.pem)?;
        match &self.source {
            PemSource::Path(path) => map.serialize_entry("path", path)?,
            PemSource::Data(data) => map.serialize_entry("data", data)?,
        }
        map.end()
    }
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Config {
    Server(ServerConfig),
    ClientConfigGroup(ClientConfigGroup),
    RuleConfigGroup(RuleConfigGroup),
    NamedPem(NamedPem),
}

impl<'de> serde::de::Deserialize<'de> for Config {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where D: serde::de::Deserializer<'de>,
    {
        use serde::de::Error;
        use serde_yml::Value;

        let value = Value::deserialize(deserializer)?;
        let map = value.as_mapping().ok_or_else(|| Error::custom("Expected mapping for Config"))?;

        if map.contains_key(Value::String("pem".to_string())) {
            serde_yml::from_value(value).map(Config::NamedPem).map_err(Error::custom)
        } else if map.contains_key(Value::String("client_group".to_string())) {
            serde_yml::from_value(value).map(Config::ClientConfigGroup).map_err(Error::custom)
        } else if map.contains_key(Value::String("rule_group".to_string())) {
            serde_yml::from_value(value).map(Config::RuleConfigGroup).map_err(Error::custom)
        } else {
            serde_yml::from_value(value).map(Config::Server).map_err(Error::custom)
        }
    }
}

impl serde::ser::Serialize for Config {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where S: serde::ser::Serializer,
    {
        match self {
            Config::Server(server) => server.serialize(serializer),
            Config::ClientConfigGroup(group) => group.serialize(serializer),
            Config::RuleConfigGroup(group) => group.serialize(serializer),
            Config::NamedPem(pem) => pem.serialize(serializer),
        }
    }
}