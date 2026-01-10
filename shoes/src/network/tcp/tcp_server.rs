use std::net::SocketAddr;
use std::sync::Arc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info};

use crate::utils::address::NetLocation;
use crate::network::async_stream::{AsyncMessageStream, AsyncStream};
use crate::client_proxy_selector::{ClientProxySelector, ConnectDecision};
use crate::config::{BindLocation, Config, ServerConfig};
use crate::utils::resolver::Resolver;
use crate::network::tcp::tcp_handler::TcpServerSetupResult;
use crate::network::tcp::tcp_server_handler_factory::create_tcp_server_handler;

pub async fn setup_client_tcp_stream(
    _server_stream: &mut Box<dyn AsyncStream>,
    proxy_selector: Arc<ClientProxySelector>,
    resolver: Arc<dyn Resolver>,
    remote_location: NetLocation,
) -> std::io::Result<Option<Box<dyn AsyncStream>>> {
    match proxy_selector.judge(remote_location, &resolver).await? {
        ConnectDecision::Allow {
            chain_group,
            remote_location,
        } => {
            let result = chain_group.connect_tcp(remote_location, &resolver).await?;
            Ok(Some(result.client_stream))
        }
        ConnectDecision::Block => Ok(None),
    }
}

pub async fn run_udp_copy(
    mut _server_stream: Box<dyn AsyncMessageStream>,
    mut _client_stream: Box<dyn AsyncMessageStream>,
) -> std::io::Result<()> {
    Ok(())
}

pub async fn handle_tcp_stream(
    stream: tokio::net::TcpStream,
    _peer_addr: SocketAddr,
    config: Arc<ServerConfig>,
    proxy_selector: Arc<crate::client_proxy_selector::ReloadableProxySelector>,
    resolver: Arc<dyn Resolver>,
) -> std::io::Result<()> {
    let active_selector = proxy_selector.load();
    let server_handler = create_tcp_server_handler(
        config.protocol.clone(),
        &active_selector,
        &resolver,
        Some(stream.local_addr()?.ip()),
    );

    let server_stream: Box<dyn AsyncStream> = Box::new(stream);
    let setup_result = server_handler.setup_server_stream(server_stream).await?;

    match setup_result {
        TcpServerSetupResult::TcpForward {
            remote_location,
            mut stream,
            need_initial_flush,
            connection_success_response,
            initial_remote_data,
            proxy_selector,
        } => {
            let client_stream = setup_client_tcp_stream(
                &mut stream,
                proxy_selector,
                resolver,
                remote_location,
            ).await?;

            if let Some(mut client_stream) = client_stream {
                if let Some(resp) = connection_success_response {
                    crate::utils::common::write_all(&mut stream, &resp).await?;
                }
                if let Some(data) = initial_remote_data {
                    crate::utils::common::write_all(&mut client_stream, &data).await?;
                }
                crate::network::copy_bidirectional::copy_bidirectional(
                    &mut stream,
                    &mut client_stream,
                    need_initial_flush,
                    false,
                ).await?;
            }
        }
        _ => {
            debug!("Unsupported or handled TCP setup result");
        }
    }

    Ok(())
}

async fn run_tcp_server_loop(
    addr: SocketAddr,
    config: Arc<ServerConfig>,
    proxy_selector: Arc<crate::client_proxy_selector::ReloadableProxySelector>,
    resolver: Arc<dyn Resolver>,
) -> std::io::Result<()> {
    let listener = crate::network::socket_util::new_tcp_listener(addr, 1024, None)?;
    info!("TCP server listening on {}", addr);

    loop {
        let (stream, peer_addr) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                error!("Accept error: {}", e);
                continue;
            }
        };

        let config = config.clone();
        let proxy_selector = proxy_selector.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_tcp_stream(stream, peer_addr, config, proxy_selector, resolver).await {
                debug!("Handle TCP stream error: {}", e);
            }
        });
    }
}

pub async fn start_servers(
    config: Config,
    proxy_selector: Arc<crate::client_proxy_selector::ReloadableProxySelector>,
    resolver: Arc<dyn Resolver>,
) -> std::io::Result<Vec<JoinHandle<()>>> {
    match config {
        Config::Server(server_config) => {
            let mut handles = Vec::new();
            let config = Arc::new(server_config);

            if let BindLocation::Address(loc) = &config.bind_location {
                let addrs = loc.to_socket_addrs()?;
                for addr in addrs {
                    let tcp_config = config.clone();
                    let tcp_selector = proxy_selector.clone();
                    let tcp_resolver = resolver.clone();
                    handles.push(tokio::spawn(async move {
                        if let Err(e) = run_tcp_server_loop(addr, tcp_config, tcp_selector, tcp_resolver).await {
                            error!("TCP server error: {}", e);
                        }
                    }));
                }
            }
            Ok(handles)
        }
        _ => Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Unsupported config type")),
    }
}
