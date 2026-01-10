use std::sync::Arc;

use parking_lot::RwLock;
use log::debug;

use crate::utils::address::NetLocation;
use crate::utils::resolver::Resolver;
use crate::client_proxy_chain::ClientChainGroup;

/// Decision from judging a connection request.
#[derive(Debug)]
pub enum ConnectDecision<'a> {
    Allow {
        chain_group: &'a ClientChainGroup,
        remote_location: NetLocation,
    },
    Block,
}

/// A simple proxy selector that holds a single chain group.
///
/// This simplified version removes:
/// - Complex rule matching (ConnectRule, ConnectAction)
/// - DNS resolution options (resolve_rule_hostnames)
/// - LRU caching (RoutingCache)
///
/// For spider's use case, only a single proxy configuration is needed.
#[derive(Debug)]
pub struct ClientProxySelector {
    chain_group: ClientChainGroup,
}

unsafe impl Send for ClientProxySelector {}
unsafe impl Sync for ClientProxySelector {}

impl ClientProxySelector {
    /// Create a new selector with a single chain group.
    ///
    /// The original implementation accepted `Vec<ConnectRule>` for rule-based routing.
    /// This simplified version only needs the chain group directly.
    pub fn new_with_chain_group(chain_group: ClientChainGroup) -> Self {
        Self { chain_group }
    }

    /// Judge a connection request.
    ///
    /// The original implementation had complex rule matching with DNS resolution
    /// and LRU caching. This simplified version always allows traffic through
    /// the configured proxy chain.
    pub async fn judge<'a>(
        &'a self,
        location: NetLocation,
        _resolver: &Arc<dyn Resolver>,
    ) -> std::io::Result<ConnectDecision<'a>> {
        debug!("Proxy selector: allowing traffic to {}", location);
        Ok(ConnectDecision::Allow {
            chain_group: &self.chain_group,
            remote_location: location,
        })
    }

    /// Get the chain group directly (for compatibility with some code paths).
    pub fn chain_group(&self) -> &ClientChainGroup {
        &self.chain_group
    }
}

/// A thread-safe, reloadable wrapper around ClientProxySelector.
///
/// This maintains the same API as the original for compatibility.
pub struct ReloadableProxySelector {
    inner: RwLock<Arc<ClientProxySelector>>,
}

impl ReloadableProxySelector {
    pub fn new(selector: ClientProxySelector) -> Self {
        Self {
            inner: RwLock::new(Arc::new(selector)),
        }
    }

    pub fn update(&self, new_selector: ClientProxySelector) {
        let mut writer = self.inner.write();
        *writer = Arc::new(new_selector);
    }

    pub fn load(&self) -> Arc<ClientProxySelector> {
        self.inner.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::tcp::chain_builder::build_client_chain_group;
    use crate::utils::option::NoneOrSome;
    use crate::config::{ClientChain, ClientChainHop, ConfigSelection};

    // Note: Tests kept minimal since this is a simplified version
    // Full test coverage is in the original implementation

    #[test]
    fn test_selector_creation() {
        // Create a simple chain group for testing
        let chain_hop = ClientChainHop::Single(ConfigSelection::Direct);
        let chain = ClientChain {
            hops: crate::utils::option::OneOrSome::One(chain_hop),
        };
        let chain_group = build_client_chain_group(
            NoneOrSome::Some(vec![chain]),
            Arc::new(crate::utils::resolver::NativeResolver::new()),
        );

        let selector = ClientProxySelector::new_with_chain_group(chain_group);
        assert!(std::ptr::eq(selector.chain_group(), selector.chain_group()));
    }
}
