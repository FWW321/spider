use std::sync::Arc;
use crate::client_proxy_selector::{ClientProxySelector, ReloadableProxySelector};
use crate::config::{ClientChain, ClientChainHop, ClientConfig, ConfigSelection};
use crate::utils::resolver::Resolver;
use crate::utils::option::{NoneOrSome, OneOrSome};
use crate::network::tcp::chain_builder::build_client_chain_group;

/// A convenience utility for quickly swapping between proxy nodes.
///
/// Simplified version - no longer builds complex rule chains.
/// Directly updates the selector with a new chain group.
pub struct ProxySwapper {
    selector: Arc<ReloadableProxySelector>,
    resolver: Arc<dyn Resolver>,
}

impl ProxySwapper {
    /// Create a new ProxySwapper with an existing selector and resolver.
    pub fn new(selector: Arc<ReloadableProxySelector>, resolver: Arc<dyn Resolver>) -> Self {
        Self { selector, resolver }
    }

    /// Swap the current proxy to the given ClientConfig.
    ///
    /// This creates a single-hop chain containing the provided config
    /// and updates the selector.
    pub fn swap_to_node(&self, node: &ClientConfig) {
        // 1. Build a single-hop chain for the node
        let chain_hop = ClientChainHop::Single(ConfigSelection::Config(node.clone()));
        let chain = ClientChain {
            hops: OneOrSome::One(chain_hop),
        };

        // 2. Build the chain group
        let chain_group = build_client_chain_group(
            NoneOrSome::Some(vec![chain]),
            self.resolver.clone(),
        );

        // 3. Update the selector directly with the chain group
        let new_selector = ClientProxySelector::new_with_chain_group(chain_group);
        self.selector.update(new_selector);
    }

    /// Reset the selector to a direct connection (bypass proxy).
    pub fn reset_to_direct(&self) {
        // Build an empty chain group (NoneOrSome::None) which means direct connection
        let chain_group = build_client_chain_group(
            NoneOrSome::None,
            self.resolver.clone(),
        );

        let new_selector = ClientProxySelector::new_with_chain_group(chain_group);
        self.selector.update(new_selector);
    }
}
