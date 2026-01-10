//! Configuration validation - validates configs and creates final ServerConfigs.

use std::collections::HashMap;
use crate::address::NetLocationMask;
use crate::option_util::{NoneOrSome, OneOrSome};
use crate::thread_util::get_num_threads;
use crate::uuid_util::parse_uuid;
use super::pem::{embed_optional_pem_from_map, embed_pem_from_map};
use super::types::{
    ClientChain, ClientChainHop, ClientConfig, ClientProxyConfig, Config, ConfigSelection,
    PemSource, RuleActionConfig, RuleConfig, ServerConfig,
    ServerProxyConfig, 
    direct_allow_rule,
};

pub async fn create_server_configs(all_configs: Vec<Config>) -> std::io::Result<Vec<Config>> {
    let mut raw_client_groups: HashMap<String, OneOrSome<ConfigSelection<ClientConfig>>> = HashMap::new();
    raw_client_groups.insert(String::from("direct"), OneOrSome::One(ConfigSelection::Config(ClientConfig::default())));

    let mut rule_groups: HashMap<String, Vec<RuleConfig>> = HashMap::new();
    rule_groups.insert(String::from("allow-all-direct"), vec![RuleConfig {
        masks: OneOrSome::One(NetLocationMask::ANY),
        action: RuleActionConfig::Allow { override_address: None, client_chains: NoneOrSome::One(ClientChain::default()) },
    }]);

    let mut server_configs: Vec<ServerConfig> = vec![];
    let mut named_pems: HashMap<String, String> = HashMap::new();

    for config in all_configs {
        match config {
            Config::ClientConfigGroup(group) => { raw_client_groups.insert(group.client_group, group.client_proxies); }
            Config::RuleConfigGroup(group) => { rule_groups.insert(group.rule_group, group.rules.into_vec()); }
            Config::Server(cfg) => server_configs.push(cfg),
            Config::NamedPem(pem) => {
                let data = match pem.source { PemSource::Data(d) => d, _ => unreachable!() };
                named_pems.insert(pem.pem, data);
            }
        }
    }

    let mut client_groups = resolve_client_groups_topologically(raw_client_groups)?;
    for configs in client_groups.values_mut() {
        for config in configs { validate_client_config(config, &named_pems)?; }
    }

    for config in &mut server_configs {
        validate_server_config(config, &client_groups, &rule_groups, &named_pems)?;
    }

    Ok(server_configs.into_iter().map(Config::Server).collect())
}

fn resolve_client_groups_topologically(raw_groups: HashMap<String, OneOrSome<ConfigSelection<ClientConfig>>>) -> std::io::Result<HashMap<String, Vec<ClientConfig>>> {
    let mut dependencies: HashMap<String, Vec<String>> = HashMap::new();
    for (name, selections) in &raw_groups {
        let mut deps = vec![];
        for s in selections.iter() {
            if let ConfigSelection::GroupName(n) = s { deps.push(n.clone()); }
        }
        dependencies.insert(name.clone(), deps);
    }

    let sorted = topological_sort(&dependencies)?;
    let mut resolved = HashMap::new();
    for name in sorted {
        let selections = raw_groups.get(&name).unwrap();
        let mut expanded = vec![];
        for s in selections.iter() {
            match s {
                ConfigSelection::Config(c) => expanded.push(c.clone()),
                ConfigSelection::GroupName(n) => expanded.extend(resolved.get(n).cloned().unwrap()),
            }
        }
        resolved.insert(name, expanded);
    }
    Ok(resolved)
}

fn topological_sort(dependencies: &HashMap<String, Vec<String>>) -> std::io::Result<Vec<String>> {
    let mut in_degree = HashMap::new();
    let mut reverse_deps: HashMap<String, Vec<String>> = HashMap::new();
    for name in dependencies.keys() { in_degree.insert(name.clone(), 0); }
    for (name, deps) in dependencies {
        for dep in deps {
            *in_degree.entry(name.clone()).or_insert(0) += 1;
            reverse_deps.entry(dep.clone()).or_default().push(name.clone());
        }
    }
    let mut queue: Vec<_> = in_degree.iter().filter(|&(_, &d)| d == 0).map(|(n, _)| n.clone()).collect();
    let mut result = vec![];
    while let Some(node) = queue.pop() {
        result.push(node.clone());
        if let Some(deps) = reverse_deps.get(&node) {
            for d in deps {
                let deg = in_degree.get_mut(d).unwrap();
                *deg -= 1;
                if *deg == 0 { queue.push(d.clone()); }
            }
        }
    }
    if result.len() != dependencies.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "Circular dependency in client groups"));
    }
    Ok(result)
}

fn validate_server_config(cfg: &mut ServerConfig, c_groups: &HashMap<String, Vec<ClientConfig>>, r_groups: &HashMap<String, Vec<RuleConfig>>, pems: &HashMap<String, String>) -> std::io::Result<()> {
    if let Some(ref mut q) = cfg.quic_settings {
        embed_pem_from_map(&mut q.cert, pems);
        embed_pem_from_map(&mut q.key, pems);
        if q.num_endpoints == 0 { q.num_endpoints = get_num_threads(); }
    }
    ConfigSelection::replace_none_or_some_groups(&mut cfg.rules, r_groups)?;
    if cfg.rules.is_empty() { cfg.rules = direct_allow_rule(); }
    for r in cfg.rules.iter_mut() { validate_rule_config(r.unwrap_config_mut(), c_groups, pems)?; }
    validate_server_proxy_config(&mut cfg.protocol, c_groups, r_groups, pems, false)?;
    Ok(())
}

fn validate_client_config(cfg: &mut ClientConfig, pems: &HashMap<String, String>) -> std::io::Result<()> {
    if let Some(ref mut q) = cfg.quic_settings {
        embed_optional_pem_from_map(&mut q.cert, pems);
        embed_optional_pem_from_map(&mut q.key, pems);
    }
    validate_client_proxy_config(&mut cfg.protocol, pems)
}

fn validate_client_proxy_config(cfg: &mut ClientProxyConfig, pems: &HashMap<String, String>) -> std::io::Result<()> {
    match cfg {
        ClientProxyConfig::Tls(t) => {
            embed_optional_pem_from_map(&mut t.cert, pems);
            embed_optional_pem_from_map(&mut t.key, pems);
            validate_client_proxy_config(&mut t.protocol, pems)?;
        }
        ClientProxyConfig::Reality { protocol, .. } => validate_client_proxy_config(protocol, pems)?,
        ClientProxyConfig::Websocket(w) => validate_client_proxy_config(&mut w.protocol, pems)?,
        _ => {}
    }
    Ok(())
}

fn validate_server_proxy_config(cfg: &mut ServerProxyConfig, c_groups: &HashMap<String, Vec<ClientConfig>>, r_groups: &HashMap<String, Vec<RuleConfig>>, pems: &HashMap<String, String>, _inside: bool) -> std::io::Result<()> {
    match cfg {
        ServerProxyConfig::Vless { user_id, .. } | ServerProxyConfig::Vmess { user_id, .. } => { parse_uuid(user_id)?; }
        ServerProxyConfig::Tls { tls_targets, default_tls_target, .. } => {
            for t in tls_targets.values_mut() {
                embed_pem_from_map(&mut t.cert, pems);
                embed_pem_from_map(&mut t.key, pems);
                validate_server_proxy_config(&mut t.protocol, c_groups, r_groups, pems, true)?;
            }
            if let Some(t) = default_tls_target {
                embed_pem_from_map(&mut t.cert, pems);
                embed_pem_from_map(&mut t.key, pems);
                validate_server_proxy_config(&mut t.protocol, c_groups, r_groups, pems, true)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_rule_config(r: &mut RuleConfig, c_groups: &HashMap<String, Vec<ClientConfig>>, pems: &HashMap<String, String>) -> std::io::Result<()> {
    if let RuleActionConfig::Allow { ref mut client_chains, .. } = r.action {
        if client_chains.is_unspecified() { *client_chains = NoneOrSome::One(ClientChain::default()); }
        for chain in client_chains.iter_mut() {
            for hop in chain.hops.iter_mut() { validate_chain_hop(hop, c_groups, pems)?; }
            expand_client_chain(&mut chain.hops, c_groups)?;
        }
    }
    Ok(())
}

fn validate_chain_hop(hop: &mut ClientChainHop, c_groups: &HashMap<String, Vec<ClientConfig>>, pems: &HashMap<String, String>) -> std::io::Result<()> {
    match hop {
        ClientChainHop::Single(s) => validate_selection(s, c_groups, pems),
        ClientChainHop::Pool(ss) => { for s in ss.iter_mut() { validate_selection(s, c_groups, pems)?; } Ok(()) }
    }
}

fn validate_selection(s: &mut ConfigSelection<ClientConfig>, c_groups: &HashMap<String, Vec<ClientConfig>>, pems: &HashMap<String, String>) -> std::io::Result<()> {
    match s {
        ConfigSelection::Config(c) => validate_client_config(c, pems),
        ConfigSelection::GroupName(n) => { if !c_groups.contains_key(n) { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("Unknown group: {n}"))); } Ok(()) }
    }
}

fn expand_client_chain(hops: &mut OneOrSome<ClientChainHop>, c_groups: &HashMap<String, Vec<ClientConfig>>) -> std::io::Result<()> {
    let mut expanded = vec![];
    for hop in hops.iter() {
        match hop {
            ClientChainHop::Single(s) => {
                let cs = expand_selection(s, c_groups)?;
                if cs.len() == 1 { expanded.push(ClientChainHop::Single(ConfigSelection::Config(cs[0].clone()))); }
                else { expanded.push(ClientChainHop::Pool(OneOrSome::Some(cs.into_iter().map(ConfigSelection::Config).collect()))); }
            }
            ClientChainHop::Pool(ss) => {
                let mut all = vec![];
                for s in ss.iter() { all.extend(expand_selection(s, c_groups)?); }
                expanded.push(ClientChainHop::Pool(OneOrSome::Some(all.into_iter().map(ConfigSelection::Config).collect())));
            }
        }
    }
    *hops = if expanded.len() == 1 { OneOrSome::One(expanded.pop().unwrap()) } else { OneOrSome::Some(expanded) };
    Ok(())
}

fn expand_selection(s: &ConfigSelection<ClientConfig>, c_groups: &HashMap<String, Vec<ClientConfig>>) -> std::io::Result<Vec<ClientConfig>> {
    match s {
        ConfigSelection::Config(c) => Ok(vec![c.clone()]),
        ConfigSelection::GroupName(n) => Ok(c_groups.get(n).unwrap().clone()),
    }
}