//! Factory functions for creating TCP client handlers from config.

use std::sync::Arc;

use log::debug;

use crate::config::{
    ClientProxyConfig, ShadowsocksConfig, TlsClientConfig,
    WebsocketClientConfig,
};
use crate::protocols::http::HttpTcpClientHandler;
use crate::network::port_forward_handler::PortForwardClientHandler;
use crate::utils::resolver::Resolver;
use crate::utils::rustls_config::create_client_config;
use crate::protocols::shadowsocks::ShadowsocksTcpHandler;
use crate::protocols::socks::SocksTcpClientHandler;
use crate::network::tcp::chain_builder::build_client_chain_group;
use crate::network::tcp::tcp_handler::TcpClientHandler;
use crate::network::tls_client::TlsClientHandler;
use crate::protocols::trojan::TrojanTcpHandler;
use crate::utils::uuid::parse_uuid;
use crate::protocols::vless::vless_client_handler::VlessTcpClientHandler;
use crate::protocols::vmess::VmessTcpClientHandler;
use crate::network::websocket::WebsocketTcpClientHandler;

fn create_auth_credentials(
    username: Option<String>,
    password: Option<String>,
) -> Option<(String, String)> {
    match (&username, &password) {
        (None, None) => None,
        _ => Some((username.unwrap_or_default(), password.unwrap_or_default())),
    }
}

pub fn create_tcp_client_handler(
    client_proxy_config: ClientProxyConfig,
    default_sni_hostname: Option<String>,
    resolver: Arc<dyn Resolver>,
) -> Box<dyn TcpClientHandler> {
    match client_proxy_config {
        ClientProxyConfig::Direct => {
            panic!("Tried to create a direct tcp client handler");
        }
        ClientProxyConfig::Http {
            username,
            password,
            resolve_hostname,
        } => {
            let http_resolver = if resolve_hostname {
                Some(resolver.clone())
            } else {
                None
            };
            Box::new(HttpTcpClientHandler::new(
                create_auth_credentials(username, password),
                http_resolver,
            ))
        }
        ClientProxyConfig::Socks { username, password } => Box::new(SocksTcpClientHandler::new(
            create_auth_credentials(username, password),
        )),
        ClientProxyConfig::Shadowsocks {
            config,
            udp_enabled,
        } => match config {
            ShadowsocksConfig::Legacy { cipher, password } => Box::new(
                ShadowsocksTcpHandler::new_client(cipher, &password, udp_enabled),
            ),
            ShadowsocksConfig::Aead2022 { cipher, key_bytes } => Box::new(
                ShadowsocksTcpHandler::new_aead2022_client(cipher, &key_bytes, udp_enabled),
            ),
        },
        ClientProxyConfig::Vless {
            user_id,
            udp_enabled,
        } => Box::new(VlessTcpClientHandler::new(&user_id, udp_enabled)),
        ClientProxyConfig::Trojan {
            password,
            shadowsocks,
        } => Box::new(TrojanTcpHandler::new_client(&password, &shadowsocks)),
        ClientProxyConfig::Tls(tls_client_config) => {
            let TlsClientConfig {
                verify,
                server_fingerprints,
                sni_hostname,
                alpn_protocols,
                tls_buffer_size,
                protocol,
                key,
                cert,
                vision,
            } = tls_client_config;

            let sni_hostname = if sni_hostname.is_unspecified() {
                if let Some(ref hostname) = default_sni_hostname {
                    debug!(
                        "Using default sni hostname for TLS client connection: {}",
                        hostname
                    );
                }
                default_sni_hostname
            } else {
                sni_hostname.into_option()
            };

            let key_and_cert_bytes = key.zip(cert).map(|(key, cert)| {
                // Certificates are already embedded as PEM data during config validation
                let cert_bytes = cert.as_bytes().to_vec();
                let key_bytes = key.as_bytes().to_vec();

                (key_bytes, cert_bytes)
            });

            let client_config = Arc::new(create_client_config(
                verify,
                server_fingerprints.into_vec(),
                alpn_protocols.into_vec(),
                sni_hostname.is_some(),
                key_and_cert_bytes,
                false, // tls13_only
            ));

            let server_name = match sni_hostname {
                Some(s) => rustls::pki_types::ServerName::try_from(s).unwrap(),
                // This is unused, since enable_sni is false, but connect_with still requires a
                // parameter.
                None => "example.com".try_into().unwrap(),
            };

            if vision {
                let ClientProxyConfig::Vless {
                    user_id,
                    udp_enabled,
                } = protocol.as_ref()
                else {
                    // Validated when loading config
                    unreachable!();
                };
                let user_id_bytes = parse_uuid(user_id)
                    .expect("Invalid user_id UUID")
                    .into_boxed_slice();
                Box::new(TlsClientHandler::new_vision_vless(
                    client_config,
                    tls_buffer_size,
                    server_name,
                    user_id_bytes,
                    *udp_enabled,
                ))
            } else {
                let handler = create_tcp_client_handler(*protocol, None, resolver.clone());

                Box::new(TlsClientHandler::new(
                    client_config,
                    tls_buffer_size,
                    server_name,
                    handler,
                ))
            }
        }
        ClientProxyConfig::Reality {
            public_key,
            short_id,
            sni_hostname,
            cipher_suites,
            vision,
            protocol,
        } => {
            // Decode public key from base64url
            let public_key_bytes =
                crate::protocols::reality::decode_public_key(&public_key).expect("Invalid REALITY public key");

            // Decode short ID from hex string
            let short_id_bytes =
                crate::protocols::reality::decode_short_id(&short_id).expect("Invalid REALITY short_id");

            // Determine SNI hostname
            let sni_hostname = sni_hostname.or(default_sni_hostname.clone());
            let server_name = match sni_hostname {
                Some(s) => rustls::pki_types::ServerName::try_from(s)
                    .unwrap()
                    .to_owned(),
                None => {
                    panic!("REALITY client requires sni_hostname to be specified");
                }
            };

            let cipher_suites = cipher_suites.into_vec();

            if vision {
                let ClientProxyConfig::Vless {
                    user_id,
                    udp_enabled,
                } = protocol.as_ref()
                else {
                    unreachable!("Vision requires VLESS (should be validated during config load)")
                };
                let user_id_bytes = parse_uuid(user_id)
                    .expect("Invalid user_id UUID")
                    .into_boxed_slice();
                Box::new(
                    crate::protocols::reality_client::RealityClientHandler::new_vision_vless(
                        public_key_bytes,
                        short_id_bytes,
                        server_name,
                        cipher_suites,
                        user_id_bytes,
                        *udp_enabled,
                    ),
                )
            } else {
                let inner_handler = create_tcp_client_handler(*protocol, None, resolver.clone());
                Box::new(crate::protocols::reality_client::RealityClientHandler::new(
                    public_key_bytes,
                    short_id_bytes,
                    server_name,
                    cipher_suites,
                    inner_handler,
                ))
            }
        }
        ClientProxyConfig::Vmess {
            cipher,
            user_id,
            udp_enabled,
        } => Box::new(VmessTcpClientHandler::new(&cipher, &user_id, udp_enabled)),
        ClientProxyConfig::Websocket(websocket_client_config) => {
            let WebsocketClientConfig {
                matching_path,
                matching_headers,
                ping_type,
                protocol,
            } = websocket_client_config;

            let handler = create_tcp_client_handler(*protocol, None, resolver.clone());

            Box::new(WebsocketTcpClientHandler::new(
                matching_path,
                matching_headers.map(|h| h.into_iter().collect()),
                ping_type,
                handler,
            ))
        }
        ClientProxyConfig::PortForward => Box::new(PortForwardClientHandler),
    }
}
