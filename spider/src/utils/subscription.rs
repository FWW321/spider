//! Proxy subscription management utilities
//!
//! Provides parsing, deduplication, and persistent caching for multi-protocol proxy nodes,
//! with runtime configuration generation for the shoes library.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose, Engine as _};
use futures::{stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};
use url::Url;

use shoes::address::{Address, NetLocation};
use shoes::config::{
    ClientConfig, ClientProxyConfig, ShadowsocksConfig, TlsClientConfig, Transport,
    WebsocketClientConfig,
};
use shoes::option_util::{NoneOrOne, NoneOrSome, OneOrSome};

/// Proxy outbound node container
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyNode {
    #[serde(flatten)]
    pub outbound: Outbound,
}

impl ProxyNode {
    /// Extract the node's unique identifier tag
    pub fn tag(&self) -> &str {
        match &self.outbound {
            Outbound::Shadowsocks { tag, .. }
            | Outbound::Vmess { tag, .. }
            | Outbound::Vless { tag, .. }
            | Outbound::Trojan { tag, .. } => tag,
        }
    }

    /// Update the node's tag
    pub fn set_tag(&mut self, new_tag: String) {
        match &mut self.outbound {
            Outbound::Shadowsocks { tag, .. }
            | Outbound::Vmess { tag, .. }
            | Outbound::Vless { tag, .. }
            | Outbound::Trojan { tag, .. } => *tag = new_tag,
        }
    }

    /// Convert to Shoes client configuration
    pub fn to_shoes_config(&self) -> Result<ClientConfig> {
        self.outbound.to_shoes_config()
    }

    /// Compute node fingerprint for state anchoring
    pub fn fingerprint(&self) -> String {
        let json = serde_json::to_string(&self.outbound).unwrap_or_default();
        blake3::hash(json.as_bytes()).to_hex().to_string()
    }

    /// Get sort key (Tag, Server, Port)
    pub fn sort_key(&self) -> (String, String, u16) {
        let (server, port) = match &self.outbound {
            Outbound::Shadowsocks { server, server_port, .. }
            | Outbound::Vmess { server, server_port, .. }
            | Outbound::Vless { server, server_port, .. }
            | Outbound::Trojan { server, server_port, .. } => (server.clone(), *server_port),
        };
        (self.tag().to_string(), server, port)
    }
}

/// Supported proxy protocol variants
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Outbound {
    Shadowsocks {
        tag: String,
        server: String,
        server_port: u16,
        method: String,
        password: String,
    },
    Vmess {
        tag: String,
        server: String,
        server_port: u16,
        uuid: String,
        security: String,
        alter_id: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        transport: Option<V2RayTransport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tls: Option<TlsOutbound>,
    },
    Vless {
        tag: String,
        server: String,
        server_port: u16,
        uuid: String,
        flow: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        transport: Option<V2RayTransport>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tls: Option<TlsOutbound>,
    },
    Trojan {
        tag: String,
        server: String,
        server_port: u16,
        password: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        tls: Option<TlsOutbound>,
        #[serde(skip_serializing_if = "Option::is_none")]
        transport: Option<V2RayTransport>,
    },
}

// Helper methods for Outbound configuration building
impl Outbound {
    fn extract_server_addr(&self) -> (String, u16) {
        match self {
            Outbound::Shadowsocks {
                server,
                server_port,
                ..
            }
            | Outbound::Vmess {
                server,
                server_port,
                ..
            }
            | Outbound::Vless {
                server,
                server_port,
                ..
            }
            | Outbound::Trojan {
                server,
                server_port,
                ..
            } => (server.clone(), *server_port),
        }
    }

    fn build_net_location(server: &str, port: u16) -> NetLocation {
        if let Ok(ip) = server.parse::<IpAddr>() {
            match ip {
                IpAddr::V4(a) => NetLocation::new(Address::Ipv4(a), port),
                IpAddr::V6(a) => NetLocation::new(Address::Ipv6(a), port),
            }
        } else {
            NetLocation::new(Address::Hostname(server.to_string()), port)
        }
    }

    fn extract_transport_config(&self) -> Option<&V2RayTransport> {
        match self {
            Outbound::Vmess { transport, .. }
            | Outbound::Vless { transport, .. }
            | Outbound::Trojan { transport, .. } => transport.as_ref(),
            _ => None,
        }
    }

    fn extract_tls_config(&self) -> Option<&TlsOutbound> {
        match self {
            Outbound::Vmess { tls, .. }
            | Outbound::Vless { tls, .. }
            | Outbound::Trojan { tls, .. } => tls.as_ref(),
            _ => None,
        }
    }

    fn build_base_protocol(&self) -> Result<ClientProxyConfig> {
        match self {
            Outbound::Shadowsocks {
                method, password, ..
            } => {
                let config = ShadowsocksConfig::from_fields(method, password)
                    .map_err(|e| anyhow!("Invalid Shadowsocks config: {}", e))?;
                Ok(ClientProxyConfig::Shadowsocks {
                    config,
                    udp_enabled: true,
                })
            }
            Outbound::Vmess { uuid, security, .. } => Ok(ClientProxyConfig::Vmess {
                cipher: security.clone(),
                user_id: uuid.clone(),
                udp_enabled: true,
            }),
            Outbound::Vless { uuid, .. } => Ok(ClientProxyConfig::Vless {
                user_id: uuid.clone(),
                udp_enabled: true,
            }),
            Outbound::Trojan { password, .. } => Ok(ClientProxyConfig::Trojan {
                password: password.clone(),
                shadowsocks: None,
            }),
        }
    }

    fn apply_websocket_layer(
        protocol: ClientProxyConfig,
        transport: &V2RayTransport,
    ) -> ClientProxyConfig {
        if let V2RayTransport::Websocket { path, headers } = transport {
            ClientProxyConfig::Websocket(WebsocketClientConfig {
                matching_path: path.clone(),
                matching_headers: headers.clone(),
                ping_type: Default::default(),
                protocol: Box::new(protocol),
            })
        } else {
            protocol
        }
    }

    fn apply_tls_layer(
        protocol: ClientProxyConfig,
        tls: &TlsOutbound,
        is_vless_vision: bool,
    ) -> ClientProxyConfig {
        let sni_hostname = match &tls.server_name {
            Some(s) => NoneOrOne::One(s.clone()),
            None => NoneOrOne::Unspecified,
        };

        let alpn_protocols = match &tls.alpn {
            Some(v) if !v.is_empty() => NoneOrSome::Some(v.clone()),
            _ => NoneOrSome::Unspecified,
        };

        let server_fingerprints = match &tls.utls {
            Some(utls) => NoneOrSome::One(utls.fingerprint.clone()),
            None => NoneOrSome::Unspecified,
        };

        ClientProxyConfig::Tls(TlsClientConfig {
            verify: !tls.insecure.unwrap_or(false),
            server_fingerprints,
            sni_hostname,
            alpn_protocols,
            tls_buffer_size: None,
            key: None,
            cert: None,
            vision: is_vless_vision,
            protocol: Box::new(protocol),
        })
    }

    fn is_vless_vision_flow(&self) -> bool {
        matches!(self, Outbound::Vless { flow, .. } if flow == "xtls-rprx-vision")
    }

    fn to_shoes_config(&self) -> Result<ClientConfig> {
        let (server, port) = self.extract_server_addr();
        let address = Self::build_net_location(&server, port);

        let mut protocol = self.build_base_protocol()?;

        // Apply WebSocket transport layer
        if let Some(transport) = self.extract_transport_config() {
            protocol = Self::apply_websocket_layer(protocol, transport);
        }

        // Apply TLS layer
        if let Some(tls) = self.extract_tls_config() {
            if tls.enabled {
                let is_vision = self.is_vless_vision_flow();

                // Early return for VLESS with Vision - special case
                if is_vision {
                    protocol = Self::apply_tls_layer(protocol, tls, true);
                    return Ok(ClientConfig {
                        bind_interface: NoneOrOne::None,
                        address,
                        protocol,
                        transport: Transport::Tcp,
                        tcp_settings: None,
                        quic_settings: None,
                    });
                }

                protocol = Self::apply_tls_layer(protocol, tls, false);
            }
        }

        Ok(ClientConfig {
            bind_interface: NoneOrOne::None,
            address,
            protocol,
            transport: Transport::Tcp,
            tcp_settings: None,
            quic_settings: None,
        })
    }
}

/// Transport layer protocols
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum V2RayTransport {
    Http {
        #[serde(skip_serializing_if = "Option::is_none")]
        host: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Websocket {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        headers: Option<HashMap<String, String>>,
    },
    Grpc {
        service_name: String,
    },
}

/// TLS/uTLS security configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsOutbound {
    pub enabled: bool,
    pub server_name: Option<String>,
    pub insecure: Option<bool>,
    pub alpn: Option<Vec<String>>,
    pub utls: Option<UtlsConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UtlsConfig {
    pub enabled: bool,
    pub fingerprint: String,
}

/// Attempt Base64 decoding with multiple encoding schemes
fn decode_base64_auto(input: &str) -> Result<String> {
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let engines = [
        &general_purpose::STANDARD,
        &general_purpose::URL_SAFE_NO_PAD,
        &general_purpose::URL_SAFE,
    ];

    for engine in engines {
        if let Ok(bytes) = engine.decode(&clean) {
            return Ok(String::from_utf8_lossy(&bytes).to_string());
        }
    }

    Err(anyhow!("Base64 decode failed"))
}

/// Parse subscription content and extract proxy nodes
pub fn parse_subscription_content(content: &str) -> Result<Vec<ProxyNode>> {
    let content = content.trim();

    // Try Clash YAML format first
    let try_parse_clash = |text: &str| -> Option<Vec<ProxyNode>> {
        if text.contains("proxies:") {
            parse_clash_yaml(text).ok().filter(|n| !n.is_empty())
        } else {
            None
        }
    };

    if let Some(nodes) = try_parse_clash(content) {
        debug!("Parsed {} nodes from raw YAML", nodes.len());
        return Ok(nodes);
    }

    // Try Base64-decoded YAML
    let decoded = decode_base64_auto(content).unwrap_or_else(|_| content.to_string());

    if let Some(nodes) = try_parse_clash(&decoded) {
        debug!("Parsed {} nodes from decoded YAML", nodes.len());
        return Ok(nodes);
    }

    // Parse URI-style protocol lines
    let nodes: Vec<ProxyNode> = decoded
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| {
            let (protocol, body) = line.split_once("://")?;
            let node = match protocol {
                "vmess" => parse_vmess(body),
                "vless" => parse_vless(line),
                "ss" => parse_ss(line),
                "trojan" => parse_trojan(line),
                _ => None,
            }?;
            is_valid_node(&node).then_some(node)
        })
        .collect();

    if nodes.is_empty() {
        Err(anyhow!("No valid proxy nodes discovered"))
    } else {
        debug!("Successfully aggregated {} nodes from URI list", nodes.len());
        Ok(nodes)
    }
}

// Protocol-specific parsers

fn json_as_u64(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

fn parse_vmess(body: &str) -> Option<ProxyNode> {
    let decoded = decode_base64_auto(body).ok()?;
    let v: Value = serde_json::from_str(&decoded).ok()?;

    let transport = match v.get("net").and_then(|s| s.as_str()) {
        Some("ws") => Some(V2RayTransport::Websocket {
            path: v.get("path").and_then(|s| s.as_str()).map(String::from),
            headers: v.get("host").and_then(|s| s.as_str()).map(|host| {
                let mut map = HashMap::new();
                map.insert("Host".to_string(), host.to_string());
                map
            }),
        }),
        _ => None,
    };

    let tls = match v.get("tls").and_then(|s| s.as_str()) {
        Some("tls") => Some(TlsOutbound {
            enabled: true,
            server_name: v.get("sni").and_then(|s| s.as_str()).map(String::from),
            insecure: Some(true),
            alpn: None,
            utls: None,
        }),
        _ => None,
    };

    Some(ProxyNode {
        outbound: Outbound::Vmess {
            tag: v.get("ps")
                .and_then(|s| s.as_str())
                .unwrap_or("vmess")
                .to_string(),
            server: v.get("add")?.as_str()?.to_string(),
            server_port: json_as_u64(v.get("port")?)? as u16,
            uuid: v.get("id")?.as_str()?.to_string(),
            security: v.get("scy")
                .and_then(|s| s.as_str())
                .unwrap_or("auto")
                .to_string(),
            alter_id: v.get("aid").and_then(json_as_u64).unwrap_or(0) as u32,
            transport,
            tls,
        },
    })
}

fn parse_ss(line: &str) -> Option<ProxyNode> {
    let url = Url::parse(line).ok()?;
    let tag = percent_encoding::percent_decode_str(url.fragment().unwrap_or("ss"))
        .decode_utf8_lossy()
        .to_string();

    // Try URL format first
    if let (Some(host), Some(port)) = (url.host_str(), url.port()) {
        let user_info = decode_base64_auto(url.username())
            .unwrap_or_else(|_| url.username().to_string());
        if let Some((method, password)) = user_info.split_once(':') {
            return Some(ProxyNode {
                outbound: Outbound::Shadowsocks {
                    tag,
                    server: host.to_string(),
                    server_port: port,
                    method: method.to_string(),
                    password: password.to_string(),
                },
            });
        }
    }

    // Try base64-encoded format
    let body = line.strip_prefix("ss://")?.split('#').next()?;
    let decoded = decode_base64_auto(body).ok()?;
    let (auth, addr) = decoded.rsplit_once('@')?;
    let (method, password) = auth.split_once(':')?;
    let (host, port_str) = addr.rsplit_once(':')?;

    Some(ProxyNode {
        outbound: Outbound::Shadowsocks {
            tag,
            server: host.to_string(),
            server_port: port_str.parse().ok()?,
            method: method.to_string(),
            password: password.to_string(),
        },
    })
}

fn parse_trojan(line: &str) -> Option<ProxyNode> {
    let url = Url::parse(line).ok()?;
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();

    let tls = Some(TlsOutbound {
        enabled: true,
        server_name: query
            .get("sni")
            .cloned()
            .or_else(|| url.host_str().map(String::from)),
        insecure: Some(true),
        alpn: None,
        utls: None,
    });

    let transport = match query.get("type").map(|s| s.as_str()) {
        Some("ws") => Some(V2RayTransport::Websocket {
            path: query.get("path").cloned(),
            headers: query.get("host").map(|host| {
                let mut map = HashMap::new();
                map.insert("Host".to_string(), host.clone());
                map
            }),
        }),
        _ => None,
    };

    Some(ProxyNode {
        outbound: Outbound::Trojan {
            tag: percent_encoding::percent_decode_str(url.fragment().unwrap_or("trojan"))
                .decode_utf8_lossy()
                .to_string(),
            server: url.host_str()?.to_string(),
            server_port: url.port()?,
            password: url.username().to_string(),
            tls,
            transport,
        },
    })
}

fn parse_vless(line: &str) -> Option<ProxyNode> {
    let url = Url::parse(line).ok()?;
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();

    let tls = match query.get("security").map(|s| s.as_str()) {
        Some("tls") | Some("xtls") => Some(TlsOutbound {
            enabled: true,
            server_name: query
                .get("sni")
                .cloned()
                .or_else(|| url.host_str().map(String::from)),
            insecure: Some(true),
            alpn: None,
            utls: query.get("fp").map(|f| UtlsConfig {
                enabled: true,
                fingerprint: f.clone(),
            }),
        }),
        _ => None,
    };

    let transport = match query.get("type").map(|s| s.as_str()) {
        Some("ws") => Some(V2RayTransport::Websocket {
            path: query.get("path").cloned(),
            headers: query.get("host").map(|host| {
                let mut map = HashMap::new();
                map.insert("Host".to_string(), host.clone());
                map
            }),
        }),
        Some("grpc") => Some(V2RayTransport::Grpc {
            service_name: query
                .get("serviceName")
                .cloned()
                .unwrap_or_default(),
        }),
        _ => None,
    };

    Some(ProxyNode {
        outbound: Outbound::Vless {
            tag: percent_encoding::percent_decode_str(url.fragment().unwrap_or("vless"))
                .decode_utf8_lossy()
                .to_string(),
            server: url.host_str()?.to_string(),
            server_port: url.port()?,
            uuid: url.username().to_string(),
            flow: query.get("flow").cloned().unwrap_or_default(),
            tls,
            transport,
        },
    })
}

/// Parse Clash YAML configuration format
fn parse_clash_yaml(content: &str) -> Result<Vec<ProxyNode>> {
    let root: Value = serde_yml::from_str(content)?;
    let proxies = root
        .get("proxies")
        .and_then(|v| v.as_array())
        .context("Missing 'proxies' key in YAML")?;

    Ok(proxies
        .iter()
        .filter_map(|proxy| {
            let tag = proxy.get("name")?.as_str()?.to_string();
            let server = proxy.get("server")?.as_str()?.to_string();
            let port = proxy.get("port")?.as_u64()? as u16;

            let transport = match proxy.get("network").and_then(|v| v.as_str()) {
                Some("ws") => Some(V2RayTransport::Websocket {
                    path: proxy
                        .get("ws-opts")
                        .and_then(|o| o.get("path"))
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    headers: proxy
                        .get("ws-opts")
                        .and_then(|o| o.get("headers"))
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                }),
                Some("grpc") => Some(V2RayTransport::Grpc {
                    service_name: proxy
                        .get("grpc-opts")
                        .and_then(|o| o.get("grpc-service-name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                _ => None,
            };

            let tls = proxy
                .get("tls")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                .then(|| TlsOutbound {
                    enabled: true,
                    server_name: proxy
                        .get("servername")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    insecure: Some(
                        proxy
                            .get("skip-cert-verify")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true),
                    ),
                    alpn: None,
                    utls: None,
                });

            let outbound = match proxy.get("type")?.as_str()? {
                "ss" => Outbound::Shadowsocks {
                    tag,
                    server,
                    server_port: port,
                    method: proxy.get("cipher")?.as_str()?.to_string(),
                    password: proxy.get("password")?.as_str()?.to_string(),
                },
                "vmess" => Outbound::Vmess {
                    tag,
                    server,
                    server_port: port,
                    uuid: proxy.get("uuid")?.as_str()?.to_string(),
                    security: proxy
                        .get("cipher")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto")
                        .to_string(),
                    alter_id: proxy.get("alterId").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    transport,
                    tls,
                },
                "vless" => Outbound::Vless {
                    tag,
                    server,
                    server_port: port,
                    uuid: proxy.get("uuid")?.as_str()?.to_string(),
                    flow: proxy
                        .get("flow")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    transport,
                    tls,
                },
                "trojan" => Outbound::Trojan {
                    tag,
                    server,
                    server_port: port,
                    password: proxy.get("password")?.as_str()?.to_string(),
                    tls,
                    transport,
                },
                _ => return None,
            };

            let node = ProxyNode { outbound };
            is_valid_node(&node).then_some(node)
        })
        .collect())
}

/// Validate node against blocklist and localhost checks
fn is_valid_node(node: &ProxyNode) -> bool {
    const BLOCKLIST: &[&str] = &[
        "广告", "官网", "流量", "重置", "群", "客服", "更新", "订阅", "expire",
    ];

    let tag = node.tag();

    if BLOCKLIST.iter().any(|&keyword| tag.contains(keyword)) {
        return false;
    }

    match &node.outbound {
        Outbound::Shadowsocks { server, .. }
        | Outbound::Vmess { server, .. }
        | Outbound::Vless { server, .. }
        | Outbound::Trojan { server, .. } => {
            server != "127.0.0.1" && server != "localhost" && !server.is_empty()
        }
    }
}

/// Fetch and parse subscription URLs with persistent caching
pub async fn fetch_subscription_urls(urls: &[String], cache_path: &Path) -> Result<Vec<ProxyNode>> {
    #[derive(Serialize, Deserialize)]
    struct Cache {
        hash: String,
        nodes: Vec<ProxyNode>,
    }

    let hash = blake3::hash(urls.join(",").as_bytes()).to_hex().to_string();
    let cache_file = cache_path.join("sub_cache.json");

    // Try to hit persistent disk cache
    if let Ok(data) = tokio::fs::read_to_string(&cache_file).await {
        if let Ok(cache) = serde_json::from_str::<Cache>(&data) {
            if cache.hash == hash {
                debug!("Cache hit for subscription: {} nodes recovered", cache.nodes.len());
                return Ok(cache.nodes);
            }
        }
    }

    // Fetch subscriptions concurrently
    let client = Arc::new(
        Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("v2rayNG/1.8.5")
            .build()?,
    );

    let fetches = stream::iter(urls.to_vec())
        .map(|url| {
            let client = client.clone();
            async move {
                debug!("Fetching subscription: {}", url);
                client
                    .get(&url)
                    .send()
                    .await
                    .map_err(|e| {
                        warn!("Request failed {}: {}", url, e);
                        e
                    })
                    .ok()?
                    .text()
                    .await
                    .map_err(|e| {
                        warn!("Content read error {}: {}", url, e);
                        e
                    })
                    .ok()
            }
        })
        .buffer_unordered(5);

    let mut all_nodes = Vec::new();
    let results: Vec<Option<String>> = fetches.collect().await;

    for content in results.into_iter().flatten() {
        if let Ok(nodes) = parse_subscription_content(&content) {
            all_nodes.extend(nodes);
        }
    }

    // Ensure deterministic ordering to prevent fingerprint variations
    all_nodes.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));

    // Apply tag deduplication with auto-suffixing
    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    for node in &mut all_nodes {
        let tag = node.tag().to_string();
        let count = tag_counts.entry(tag.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            node.set_tag(format!("{} {}", tag, count));
        }
    }

    // Persist to cache if nodes were found
    if !all_nodes.is_empty() {
        if let Some(parent) = cache_file.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let cache = Cache {
            hash,
            nodes: all_nodes.clone(),
        };
        if let Ok(json) = serde_json::to_string(&cache) {
            let _ = tokio::fs::write(cache_file, json).await;
        }
    }

    Ok(all_nodes)
}
