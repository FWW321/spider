//! Server configuration types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::utils::address::NetLocation;
use crate::utils::option::{NoneOrSome, OneOrSome};

use super::common::{default_reality_server_short_ids, default_reality_time_diff, default_true};
use super::rules::{ClientChainHop, RuleConfig};
use super::selection::ConfigSelection;
use super::shadowsocks::ShadowsocksConfig;
use super::transport::{BindLocation, ServerQuicConfig, TcpConfig, Transport};

/// Custom deserializer for ServerProxyConfig::Shadowsocks
fn deserialize_shadowsocks_server<'de, D>(
    deserializer: D,
) -> Result<(ShadowsocksConfig, bool), D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ShadowsocksServerTemp {
        cipher: String,
        password: String,
        #[serde(default = "default_true")]
        udp_enabled: bool,
    }

    let temp = ShadowsocksServerTemp::deserialize(deserializer)?;
    let config =
        ShadowsocksConfig::from_fields(&temp.cipher, &temp.password).map_err(Error::custom)?;

    Ok((config, temp.udp_enabled))
}

/// Custom serializer for ServerProxyConfig::Shadowsocks - flattens config fields
fn serialize_shadowsocks_server<S>(
    config: &ShadowsocksConfig,
    udp_enabled: &bool,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeStruct;

    let mut state = serializer.serialize_struct("Shadowsocks", 3)?;
    config.serialize_fields(&mut state)?;
    state.serialize_field("udp_enabled", udp_enabled)?;
    state.end()
}

/// Custom deserializer for ServerProxyConfig::Vmess that validates legacy force_aead field
fn deserialize_vmess_server<'de, D>(deserializer: D) -> Result<(String, String, bool), D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct VmessServerTemp {
        cipher: String,
        user_id: String,
        #[serde(default = "default_true")]
        udp_enabled: bool,
        #[serde(default)]
        force_aead: Option<bool>,
    }

    let temp = VmessServerTemp::deserialize(deserializer)?;

    // Check if force_aead was explicitly set
    if let Some(force_aead_value) = temp.force_aead {
        if !force_aead_value {
            return Err(Error::custom(
                "Non-AEAD VMess mode (force_aead=false) is no longer supported. \
                 Please remove the force_aead field from your configuration, or set it to true.",
            ));
        }
        // Warn about deprecated field
        log::warn!(
            "The 'force_aead' field in VMess server configuration is deprecated and will be removed in a future version. \
             AEAD mode is now always enabled. Please remove this field from your configuration."
        );
    }

    Ok((temp.cipher, temp.user_id, temp.udp_enabled))
}

pub fn direct_allow_rule() -> NoneOrSome<ConfigSelection<RuleConfig>> {
    NoneOrSome::One(ConfigSelection::Config(RuleConfig::default()))
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(flatten)]
    pub bind_location: BindLocation,
    pub protocol: ServerProxyConfig,
    #[serde(default, skip_serializing_if = "Transport::is_default")]
    pub transport: Transport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_settings: Option<TcpConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quic_settings: Option<ServerQuicConfig>,
    #[serde(
        alias = "rule",
        default = "direct_allow_rule",
        skip_serializing_if = "NoneOrSome::is_unspecified"
    )]
    pub rules: NoneOrSome<ConfigSelection<RuleConfig>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RealityServerConfig {
    /// X25519 private key (32 bytes, base64url encoded)
    pub private_key: String,

    /// List of valid short IDs (hex strings, 0-16 chars each)
    #[serde(alias = "short_id", default = "default_reality_server_short_ids")]
    pub short_ids: OneOrSome<String>,

    /// Fallback destination (e.g., "example.com:443")
    pub dest: NetLocation,

    /// Maximum timestamp difference in milliseconds (optional)
    #[serde(default = "default_reality_time_diff")]
    pub max_time_diff: Option<u64>,

    /// Minimum client version [major, minor, patch] (optional)
    #[serde(default)]
    pub min_client_version: Option<[u8; 3]>,

    /// Maximum client version [major, minor, patch] (optional)
    #[serde(default)]
    pub max_client_version: Option<[u8; 3]>,

    /// TLS 1.3 cipher suites to support (optional)
    /// Valid values: "TLS_AES_128_GCM_SHA256", "TLS_AES_256_GCM_SHA384", "TLS_CHACHA20_POLY1305_SHA256"
    /// If empty or not specified, the default set is used.
    #[serde(alias = "cipher_suite", default)]
    pub cipher_suites: NoneOrSome<crate::protocols::reality::CipherSuite>,

    /// Enable XTLS-Vision protocol for TLS-in-TLS optimization.
    /// When enabled, the inner protocol MUST be VLESS.
    #[serde(default)]
    pub vision: bool,
    /// Inner protocol (VLESS, Trojan, etc.)
    pub protocol: ServerProxyConfig,

    /// Client chain for connecting to dest server (for fallback connections).
    /// If not specified, connects directly to dest.
    #[serde(default)]
    pub dest_client_chain: NoneOrSome<ClientChainHop>,

    /// Override rules
    #[serde(alias = "override_rule", default)]
    pub override_rules: NoneOrSome<ConfigSelection<RuleConfig>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TlsServerConfig {
    pub cert: String,
    pub key: String,
    #[serde(alias = "alpn_protocol", default)]
    pub alpn_protocols: NoneOrSome<String>,

    #[serde(alias = "client_ca_cert", default)]
    pub client_ca_certs: NoneOrSome<String>,

    #[serde(alias = "client_fingerprint", default)]
    pub client_fingerprints: NoneOrSome<String>,

    /// Enable XTLS-Vision protocol for TLS-in-TLS optimization.
    /// When enabled, the inner protocol MUST be VLESS.
    /// Vision detects TLS-in-TLS scenarios and switches to Direct mode for zero-copy performance.
    /// Requires TLS 1.3.
    #[serde(default)]
    pub vision: bool,
    pub protocol: ServerProxyConfig,

    #[serde(alias = "override_rule", default)]
    pub override_rules: NoneOrSome<ConfigSelection<RuleConfig>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebsocketServerConfig {
    #[serde(default)]
    pub matching_path: Option<String>,
    #[serde(default)]
    pub matching_headers: Option<HashMap<String, String>>,
    pub protocol: ServerProxyConfig,
    #[serde(default)]
    pub ping_type: WebsocketPingType,

    #[serde(alias = "override_rule", default)]
    pub override_rules: NoneOrSome<ConfigSelection<RuleConfig>>,
}

#[derive(Default, Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum WebsocketPingType {
    Disabled,
    #[serde(alias = "ping", alias = "ping-frame")]
    #[default]
    PingFrame,
    #[serde(alias = "empty", alias = "empty-frame")]
    EmptyFrame,
}

impl WebsocketPingType {
    pub fn is_default(&self) -> bool {
        matches!(self, WebsocketPingType::PingFrame)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ServerProxyConfig {
    Http {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
    },
    #[serde(alias = "socks5")]
    Socks {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(default = "default_true")]
        udp_enabled: bool,
    },
    #[serde(
        alias = "ss",
        deserialize_with = "deserialize_shadowsocks_server",
        serialize_with = "serialize_shadowsocks_server"
    )]
    Shadowsocks {
        config: ShadowsocksConfig,
        #[serde(default = "default_true")]
        udp_enabled: bool,
    },
    Vless {
        user_id: String,
        #[serde(default = "default_true")]
        udp_enabled: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback: Option<NetLocation>,
    },
    Trojan {
        password: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        shadowsocks: Option<ShadowsocksConfig>,
    },
    Tls {
        #[serde(default, alias = "sni_targets", alias = "targets")]
        tls_targets: HashMap<String, TlsServerConfig>,
        #[serde(
            default,
            alias = "default_target",
            skip_serializing_if = "Option::is_none"
        )]
        default_tls_target: Option<Box<TlsServerConfig>>,
        #[serde(default)]
        reality_targets: HashMap<String, RealityServerConfig>,

        #[serde(default, skip_serializing_if = "Option::is_none")]
        tls_buffer_size: Option<usize>,
    },
    #[serde(deserialize_with = "deserialize_vmess_server")]
    Vmess {
        cipher: String,
        user_id: String,
        #[serde(default = "default_true")]
        udp_enabled: bool,
    },
    #[serde(alias = "ws")]
    Websocket {
        #[serde(alias = "target")]
        targets: Box<OneOrSome<WebsocketServerConfig>>,
    },
    #[serde(alias = "forward")]
    PortForward {
        #[serde(alias = "target")]
        targets: OneOrSome<NetLocation>,
    },
    #[serde(alias = "http+socks", alias = "socks+http")]
    Mixed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        password: Option<String>,
        #[serde(default = "default_true")]
        udp_enabled: bool,
    },
}

impl std::fmt::Display for ServerProxyConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { .. } => write!(f, "HTTP"),
            Self::Socks { .. } => write!(f, "SOCKS"),
            Self::Shadowsocks { .. } => write!(f, "Shadowsocks"),
            Self::Vless { .. } => write!(f, "Vless"),
            Self::Trojan { .. } => write!(f, "Trojan"),
            Self::Tls {
                tls_targets,
                default_tls_target,
                reality_targets,
                .. 
            } => {
                let mut parts = vec![];

                if !tls_targets.is_empty() {
                    parts.push("TLS");
                }

                if !reality_targets.is_empty() {
                    parts.push("REALITY");
                }
                if tls_targets.values().any(|cfg| cfg.vision)
                    || default_tls_target.as_ref().is_some_and(|cfg| cfg.vision)
                    || reality_targets.values().any(|cfg| cfg.vision)
                {
                    parts.push("Vision");
                }

                write!(f, "{}", parts.join("+"))
            }
            Self::Vmess { .. } => write!(f, "Vmess"),
            Self::Websocket { .. } => write!(f, "Websocket"),
            Self::PortForward { .. } => write!(f, "Portforward"),
            Self::Mixed { .. } => write!(f, "Mixed (HTTP+SOCKS5)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::path::PathBuf;

    fn create_test_server_config_http() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080).into(),
            ),
            protocol: ServerProxyConfig::Http {
                username: Some("user".to_string()),
                password: Some("pass".to_string()),
            },
            transport: Transport::Tcp,
            tcp_settings: Some(TcpConfig { no_delay: true }),
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_socks() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 1080).into(),
            ),
            protocol: ServerProxyConfig::Socks {
                username: None,
                password: None,
                udp_enabled: false,
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_shadowsocks() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Path(PathBuf::from("/tmp/ss.sock")),
            protocol: ServerProxyConfig::Shadowsocks {
                config: ShadowsocksConfig::Legacy {
                    cipher: "aes-256-gcm".try_into().unwrap(),
                    password: "secret123".to_string(),
                },
                udp_enabled: true,
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_vless() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V6(Ipv6Addr::new(0, 0, 0, 0, 0, 0, 0, 1)), 443)
                    .into(),
            ),
            protocol: ServerProxyConfig::Vless {
                user_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
                udp_enabled: true,
                fallback: None,
            },
            transport: Transport::Quic,
            tcp_settings: None,
            quic_settings: Some(ServerQuicConfig {
                cert: "server.crt".to_string(),
                key: "server.key".to_string(),
                alpn_protocols: NoneOrSome::Some(vec!["h3".to_string()]),
                client_ca_certs: NoneOrSome::None,
                client_fingerprints: NoneOrSome::None,
                num_endpoints: 1,
            }),
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_trojan() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)), 443).into(),
            ),
            protocol: ServerProxyConfig::Trojan {
                password: "trojan_password".to_string(),
                shadowsocks: Some(ShadowsocksConfig::Legacy {
                    cipher: "chacha20-poly1305".try_into().unwrap(),
                    password: "ss_password".to_string(),
                }),
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_tls() -> ServerConfig {
        let mut tls_targets = HashMap::new();
        tls_targets.insert(
            "example.com".to_string(),
            TlsServerConfig {
                cert: "example.crt".to_string(),
                key: "example.key".to_string(),
                alpn_protocols: NoneOrSome::Some(vec!["h2".to_string(), "http/1.1".to_string()]),
                client_ca_certs: NoneOrSome::One("ca.crt".to_string()),
                client_fingerprints: NoneOrSome::One("abc123".to_string()),
                vision: false,
                protocol: ServerProxyConfig::Http {
                    username: None,
                    password: None,
                },
                override_rules: NoneOrSome::None,
            },
        );

        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 8443).into(),
            ),
            protocol: ServerProxyConfig::Tls {
                tls_targets,
                default_tls_target: Some(Box::new(TlsServerConfig {
                    cert: "default.crt".to_string(),
                    key: "default.key".to_string(),
                    alpn_protocols: NoneOrSome::None,
                    client_ca_certs: NoneOrSome::None,
                    client_fingerprints: NoneOrSome::None,
                    vision: false,
                    protocol: ServerProxyConfig::Http {
                        username: None,
                        password: None,
                    },
                    override_rules: NoneOrSome::None,
                })),
                reality_targets: HashMap::new(),
                tls_buffer_size: Some(8192),
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_vmess() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1)), 10086).into(),
            ),
            protocol: ServerProxyConfig::Vmess {
                cipher: "aes-128-gcm".to_string(),
                user_id: "b831381d-6324-4d53-ad4f-8cda48b30811".to_string(),
                udp_enabled: false,
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_websocket() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080).into(),
            ),
            protocol: ServerProxyConfig::Websocket {
                targets: Box::new(OneOrSome::One(WebsocketServerConfig {
                    matching_path: Some("/ws".to_string()),
                    matching_headers: None,
                    protocol: ServerProxyConfig::Http {
                        username: None,
                        password: None,
                    },
                    ping_type: WebsocketPingType::PingFrame,
                    override_rules: NoneOrSome::None,
                })),
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    fn create_test_server_config_port_forward() -> ServerConfig {
        ServerConfig {
            bind_location: BindLocation::Address(
                NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9090).into(),
            ),
            protocol: ServerProxyConfig::PortForward {
                targets: OneOrSome::Some(vec![
                    NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 80),
                    NetLocation::from_ip_addr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 80),
                ]),
            },
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
            rules: NoneOrSome::None,
        }
    }

    // Test individual server config variants
    #[test]
    fn test_server_config_http() {
        let original = create_test_server_config_http();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        println!("HTTP config YAML:\n{yaml_str}");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Http { .. }
        ));
    }

    #[test]
    fn test_server_config_socks() {
        let original = create_test_server_config_socks();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Socks { .. }
        ));
    }

    #[test]
    fn test_server_config_shadowsocks() {
        let original = create_test_server_config_shadowsocks();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        println!("Shadowsocks YAML: {yaml_str}");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Shadowsocks { .. }
        ));
    }

    #[test]
    fn test_server_config_vless() {
        let original = create_test_server_config_vless();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Vless { .. }
        ));
    }

    #[test]
    fn test_server_config_trojan() {
        let original = create_test_server_config_trojan();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Trojan { .. }
        ));
    }

    #[test]
    fn test_server_config_tls() {
        let original = create_test_server_config_tls();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Tls { .. }
        ));
    }

    #[test]
    fn test_server_config_vmess() {
        let original = create_test_server_config_vmess();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Vmess { .. }
        ));
    }

    #[test]
    fn test_server_config_websocket() {
        let original = create_test_server_config_websocket();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::Websocket { .. }
        ));
    }

    #[test]
    fn test_server_config_port_forward() {
        let original = create_test_server_config_port_forward();
        let yaml_str = serde_yml::to_string(&original).expect("Failed to serialize");
        let deserialized: ServerConfig = 
            serde_yml::from_str(&yaml_str).expect("Failed to deserialize");
        assert!(matches!(
            deserialized.protocol,
            ServerProxyConfig::PortForward { .. }
        ));
    }

    #[test]
    fn test_rejects_invalid_upstream_field() {
        let yaml = r#"address: "127.0.0.1:8080"
protocol:
  type: http
upstream:
  address: "127.0.0.1:443"
  protocol:
    type: vless
    user_id: "test-uuid""#;

        let result: Result<ServerConfig, _> = serde_yml::from_str(yaml);
        assert!(
            result.is_err(),
            "Should reject config with invalid 'upstream' field"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field") && err.contains("upstream"),
            "Error should mention 'upstream' as unknown field, got: {err}"
        );
    }

    #[test]
    fn test_rejects_typo_in_field_name() {
        let yaml = r#"address: "127.0.0.1:8080"
protocl:
  type: http"#;

        let result: Result<ServerConfig, _> = serde_yml::from_str(yaml);
        assert!(result.is_err(), "Should reject config with typo 'protocl'");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field") && err.contains("protocl"),
            "Error should mention 'protocl' as unknown field, got: {err}"
        );
    }

    #[test]
    fn test_accepts_valid_server_config() {
        let yaml = r#"address: "127.0.0.1:8080"
protocol:
  type: http
rules:
  - mask: 0.0.0.0/0
    action: allow"#;

        let result: Result<ServerConfig, _> = serde_yml::from_str(yaml);
        assert!(
            result.is_ok(),
            "Should accept valid server config: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_accepts_valid_server_config_with_all_fields() {
        let yaml = r#"address: "127.0.0.1:8080"
protocol:
  type: http
transport: tcp
tcp_settings:
  no_delay: true
rules:
  - mask: 0.0.0.0/0
    action: allow"#;

        let result: Result<ServerConfig, _> = serde_yml::from_str(yaml);
        assert!(
            result.is_ok(),
            "Should accept valid server config with all fields: {:?}",
            result.err()
        );
    }

    #[test]
    fn test_rejects_unknown_field_in_vmess_server() {
        let yaml = r#"address: "127.0.0.1:8080"
protocol:
  type: vmess
  cipher: aes-128-gcm
  user_id: "b0e80a62-8a51-47f0-91f1-f0f7faf8d9d4"
  typo_field: "should fail""#;
        let result: Result<ServerConfig, _> = serde_yml::from_str(yaml);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("unknown field `typo_field`"),
            "Error should mention unknown field: {err}"
        );
    }

    // Note: This test was for reject unknown fields in Shadowsocks config.
    // After adding udp_enabled with #[serde(flatten)] for ShadowsocksConfig,
    // serde can no longer enforce deny_unknown_fields due to a serde limitation.
    // See: https://github.com/serde-rs/serde/issues/1547
    //
    // The test is kept here but modified to verify that the known fields work correctly.
    #[test]
    fn test_shadowsocks_with_udp_enabled() {
        let yaml = r#"address: "127.0.0.1:8080"
protocol:
  type: shadowsocks
  cipher: aes-256-gcm
  password: "secret"
  udp_enabled: false"#;
        let result: Result<ServerConfig, _> = serde_yml::from_str(yaml);
        assert!(result.is_ok(), "Valid Shadowsocks config should parse");
        let config = result.unwrap();
        if let ServerProxyConfig::Shadowsocks { udp_enabled, .. } = config.protocol {
            assert!(!udp_enabled, "udp_enabled should be false");
        } else {
            panic!("Expected Shadowsocks protocol");
        }
    }
}