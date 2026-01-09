use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose};
use futures::{StreamExt, stream};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};
use url::Url;

// --- Data Structures ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyNode {
    #[serde(flatten)]
    pub outbound: Outbound,
}

impl ProxyNode {
    pub fn tag(&self) -> &str {
        match &self.outbound {
            Outbound::Shadowsocks { tag, .. }
            | Outbound::Vmess { tag, .. }
            | Outbound::Vless { tag, .. }
            | Outbound::Trojan { tag, .. } => tag,
        }
    }

    pub fn set_tag(&mut self, new_tag: String) {
        match &mut self.outbound {
            Outbound::Shadowsocks { tag, .. }
            | Outbound::Vmess { tag, .. }
            | Outbound::Vless { tag, .. }
            | Outbound::Trojan { tag, .. } => *tag = new_tag,
        }
    }
}

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

fn decode_base64_auto(input: &str) -> Result<String> {
    let clean: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    // 优先级: Standard -> URL_Safe_No_Pad -> URL_Safe
    let engines = [
        &general_purpose::STANDARD,
        &general_purpose::URL_SAFE_NO_PAD,
        &general_purpose::URL_SAFE,
    ];

    for engine in engines {
        if let Ok(b) = engine.decode(&clean) {
            return Ok(String::from_utf8_lossy(&b).to_string());
        }
    }
    Err(anyhow!("Base64 decode failed"))
}

pub fn parse_subscription_content(content: &str) -> Result<Vec<ProxyNode>> {
    let content = content.trim();

    let try_clash = |text: &str| -> Option<Vec<ProxyNode>> {
        if text.contains("proxies:") {
            parse_clash_yaml(text).ok().filter(|n| !n.is_empty())
        } else {
            None
        }
    };

    if let Some(nodes) = try_clash(content) {
        debug!("成功从 YAML 解析 {} 个节点", nodes.len());
        return Ok(nodes);
    }

    let decoded = decode_base64_auto(content).unwrap_or_else(|_| content.to_string());

    if let Some(nodes) = try_clash(&decoded) {
        debug!("成功从 Base64 解码的 YAML 中解析 {} 个节点", nodes.len());
        return Ok(nodes);
    }

    let nodes: Vec<ProxyNode> = decoded
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
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
        Err(anyhow!("未发现有效的代理节点"))
    } else {
        debug!("成功解析链接列表: {} 个节点", nodes.len());
        Ok(nodes)
    }
}

// --- Protocol Parsers ---

// 辅助函数：处理 json 中可能是字符串也可能是数字的字段
fn json_as_u64(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}

fn parse_vmess(body: &str) -> Option<ProxyNode> {
    let decoded = decode_base64_auto(body).ok()?;
    let v: Value = serde_json::from_str(&decoded).ok()?;

    let transport = match v.get("net").and_then(|s| s.as_str()) {
        Some("ws") => Some(V2RayTransport::Websocket {
            path: v
                .get("path")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string()),
            headers: v.get("host").and_then(|s| s.as_str()).map(|h| {
                let mut m = HashMap::new();
                m.insert("Host".to_string(), h.to_string());
                m
            }),
        }),
        _ => None,
    };

    let tls = match v.get("tls").and_then(|s| s.as_str()) {
        Some("tls") => Some(TlsOutbound {
            enabled: true,
            server_name: v.get("sni").and_then(|s| s.as_str()).map(|s| s.to_string()),
            insecure: Some(true),
            alpn: None,
            utls: None,
        }),
        _ => None,
    };

    Some(ProxyNode {
        outbound: Outbound::Vmess {
            tag: v
                .get("ps")
                .and_then(|s| s.as_str())
                .unwrap_or("vmess")
                .to_string(),
            server: v.get("add")?.as_str()?.to_string(),
            server_port: json_as_u64(v.get("port")?)? as u16,
            uuid: v.get("id")?.as_str()?.to_string(),
            security: v
                .get("scy")
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

    // 尝试 SIP002 (ss://user:pass@host:port)
    if let (Some(host), Some(port)) = (url.host_str(), url.port()) {
        let user_info =
            decode_base64_auto(url.username()).unwrap_or_else(|_| url.username().to_string());
        let (method, password) = user_info.split_once(':')?;
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

    // 尝试 Legacy (ss://BASE64)
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
    let query: HashMap<_, _> = url.query_pairs().collect();

    let tls = Some(TlsOutbound {
        enabled: true,
        server_name: query
            .get("sni")
            .map(|s| s.to_string())
            .or_else(|| url.host_str().map(|s| s.to_string())),
        insecure: Some(true),
        alpn: None,
        utls: None,
    });

    let transport = match query.get("type").map(|s| s.as_ref()) {
        Some("ws") => Some(V2RayTransport::Websocket {
            path: query.get("path").map(|s| s.to_string()),
            headers: query.get("host").map(|h| {
                let mut m = HashMap::new();
                m.insert("Host".to_string(), h.to_string());
                m
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
    let query: HashMap<_, _> = url.query_pairs().collect();

    let tls = match query.get("security").map(|s| s.as_ref()) {
        Some("tls") | Some("xtls") => Some(TlsOutbound {
            enabled: true,
            server_name: query
                .get("sni")
                .map(|s| s.to_string())
                .or_else(|| url.host_str().map(|s| s.to_string())),
            insecure: Some(true),
            alpn: None,
            utls: query.get("fp").map(|f| UtlsConfig {
                enabled: true,
                fingerprint: f.to_string(),
            }),
        }),
        _ => None,
    };

    let transport = match query.get("type").map(|s| s.as_ref()) {
        Some("ws") => Some(V2RayTransport::Websocket {
            path: query.get("path").map(|s| s.to_string()),
            headers: query.get("host").map(|h| {
                let mut m = HashMap::new();
                m.insert("Host".to_string(), h.to_string());
                m
            }),
        }),
        Some("grpc") => Some(V2RayTransport::Grpc {
            service_name: query
                .get("serviceName")
                .map(|s| s.to_string())
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
            flow: query.get("flow").map(|s| s.to_string()).unwrap_or_default(),
            tls,
            transport,
        },
    })
}

fn parse_clash_yaml(content: &str) -> Result<Vec<ProxyNode>> {
    let root: Value = serde_yml::from_str(content)?;
    let proxies = root
        .get("proxies")
        .and_then(|v| v.as_array())
        .context("No proxies found")?;

    Ok(proxies
        .iter()
        .filter_map(|p| {
            let tag = p.get("name")?.as_str()?.to_string();
            let server = p.get("server")?.as_str()?.to_string();
            let port = p.get("port")?.as_u64()? as u16;

            let transport = match p.get("network").and_then(|v| v.as_str()) {
                Some("ws") => Some(V2RayTransport::Websocket {
                    path: p
                        .get("ws-opts")
                        .and_then(|o| o.get("path"))
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    headers: p
                        .get("ws-opts")
                        .and_then(|o| o.get("headers"))
                        .and_then(|v| serde_json::from_value(v.clone()).ok()),
                }),
                Some("grpc") => Some(V2RayTransport::Grpc {
                    service_name: p
                        .get("grpc-opts")
                        .and_then(|o| o.get("grpc-service-name"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string(),
                }),
                _ => None,
            };

            let tls = p
                .get("tls")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                .then(|| TlsOutbound {
                    enabled: true,
                    server_name: p
                        .get("servername")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    insecure: Some(
                        p.get("skip-cert-verify")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true),
                    ),
                    alpn: None,
                    utls: None,
                });

            let outbound = match p.get("type")?.as_str()? {
                "ss" => Outbound::Shadowsocks {
                    tag,
                    server,
                    server_port: port,
                    method: p.get("cipher")?.as_str()?.to_string(),
                    password: p.get("password")?.as_str()?.to_string(),
                },
                "vmess" => Outbound::Vmess {
                    tag,
                    server,
                    server_port: port,
                    uuid: p.get("uuid")?.as_str()?.to_string(),
                    security: p
                        .get("cipher")
                        .and_then(|v| v.as_str())
                        .unwrap_or("auto")
                        .to_string(),
                    alter_id: p.get("alterId").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                    transport,
                    tls,
                },
                "vless" => Outbound::Vless {
                    tag,
                    server,
                    server_port: port,
                    uuid: p.get("uuid")?.as_str()?.to_string(),
                    flow: p
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
                    password: p.get("password")?.as_str()?.to_string(),
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

fn is_valid_node(node: &ProxyNode) -> bool {
    const BLOCKLIST: &[&str] = &[
        "广告", "官网", "流量", "重置", "群", "客服", "更新", "订阅", "expire",
    ];
    let tag = node.tag();

    // 检查关键字
    if BLOCKLIST.iter().any(|&k| tag.contains(k)) {
        return false;
    }

    // 检查本地地址
    match &node.outbound {
        Outbound::Shadowsocks { server, .. }
        | Outbound::Vmess { server, .. }
        | Outbound::Vless { server, .. }
        | Outbound::Trojan { server, .. } => server != "127.0.0.1" && server != "localhost",
    }
}

// --- Modern Async Fetching ---

pub async fn fetch_subscription_urls(urls: &[String], cache_path: &Path) -> Result<Vec<ProxyNode>> {
    #[derive(Serialize, Deserialize)]
    struct Cache {
        hash: String,
        nodes: Vec<ProxyNode>,
    }

    let hash = blake3::hash(urls.join(",").as_bytes()).to_hex().to_string();
    let cache_file = cache_path.join("sub_cache.json");

    if let Ok(data) = tokio::fs::read_to_string(&cache_file).await
        && let Ok(cache) = serde_json::from_str::<Cache>(&data)
            && cache.hash == hash {
                debug!("订阅缓存命中: {} 个节点", cache.nodes.len());
                return Ok(cache.nodes);
            }

    let client = Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("v2rayNG/1.8.5")
        .build()?;
    let client = Arc::new(client);

    let fetches = stream::iter(urls)
        .map(|url| {
            let client = client.clone();
            async move {
                debug!("正在获取订阅: {}", url);
                client
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| {
                        warn!("订阅请求失败 {}: {}", url, e);
                        e
                    })
                    .ok()?
                    .text()
                    .await
                    .map_err(|e| {
                        warn!("读取订阅内容失败 {}: {}", url, e);
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

    let mut counts: HashMap<String, usize> = HashMap::new();
    for node in &mut all_nodes {
        let tag = node.tag().to_string();
        let count = counts.entry(tag.clone()).or_insert(0);
        *count += 1;
        if *count > 1 {
            node.set_tag(format!("{} {}", tag, count));
        }
    }

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

pub fn generate_singbox_config(
    nodes: &[ProxyNode],
    proxy_port: u16,
    api_port: u16,
    api_secret: &str,
    cache_path: &Path,
) -> Result<String> {
    if nodes.is_empty() {
        return Err(anyhow!("No proxy nodes"));
    }

    let node_tags: Vec<String> = nodes.iter().map(|n| n.tag().to_string()).collect();

    let mut outbound_list = vec![
        json!({ "type": "direct", "tag": "direct" }),
        json!({
            "type": "selector",
            "tag": "proxy_selector",
            "outbounds": node_tags,
            "default": node_tags.first().unwrap_or(&"direct".to_string()),
            "interrupt_exist_connections": true
        }),
    ];

    // 批量转换节点并添加到列表
    for node in nodes {
        outbound_list.push(serde_json::to_value(&node.outbound)?);
    }

    let config = json!({
        "log": { "level": "info" },
        "dns": {
            "servers": [{ "type": "https", "tag": "dns-local", "server": "223.5.5.5" }],
            "final": "dns-local"
        },
        "inbounds": [{
            "type": "mixed", "tag": "proxy-in", "listen": "127.0.0.1", "listen_port": proxy_port
        }],
        "outbounds": outbound_list,
        "route": {
            "rules": [
                { "protocol": "dns", "outbound": "direct" },
                { "outbound": "proxy_selector", "inbound": "proxy-in" }
            ],
            "final": "direct",
            "auto_detect_interface": true
        },
        "experimental": {
            "clash_api": {
                "external_controller": format!("127.0.0.1:{}", api_port),
                "secret": api_secret
            },
            "cache_file": {
                "enabled": true,
                "path": cache_path.join("cache.db").to_str()
            }
        }
    });

    serde_json::to_string_pretty(&config).context("Serialize config failed")
}
