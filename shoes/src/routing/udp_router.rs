//! UDP routing module for per-destination routing.

use std::sync::Arc;

pub enum ServerStream {
    Targeted(Box<dyn crate::async_stream::AsyncTargetedMessageStream>),
    Session(Box<dyn crate::async_stream::AsyncSessionMessageStream>),
}

pub async fn run_udp_routing(
    _stream: ServerStream,
    _proxy_selector: Arc<crate::client_proxy_selector::ClientProxySelector>,
    _resolver: Arc<dyn crate::resolver::Resolver>,
    _allow_direct: bool,
) -> std::io::Result<()> {
    Ok(())
}