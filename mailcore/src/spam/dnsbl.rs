//! DNSBL/RBL lookups: reverse the IP or hostname and query the zone. A returned
//! address means the entry is listed, which becomes an RBL_* tag.

use crate::spam::Tags;
use crate::spam::checks::Elements;
use crate::spam::checks::tag;
use crate::spam::config::SpamConfig;
use crate::spam::state::StateStore;
use mail_auth::hickory_resolver::TokioResolver;
use std::net::IpAddr;

pub async fn check(
    resolver: Option<&TokioResolver>,
    peer: IpAddr,
    elements: &Elements,
    config: &SpamConfig,
    tags: &mut Tags,
    state: &StateStore,
) {
    let Some(resolver) = resolver else {
        return;
    };
    for server in &config.dnsbl {
        let scope = server.scope.to_ascii_lowercase();
        let cap = server.max_checks.max(1) as usize;
        if scope == "ip" {
            if let Some(name) = reverse_ip(peer, &server.zone)
                && listed(resolver, &name, state).await
            {
                tag(tags, format!("RBL_{}", zone_tag(&server.zone)));
            }
        } else if scope == "domain" || scope == "url" {
            for domain in elements.domains.iter().take(cap) {
                let name = format!("{domain}.{}", server.zone);
                if listed(resolver, &name, state).await {
                    tag(tags, format!("RBL_{}", zone_tag(&server.zone)));
                }
            }
        }
    }
}

async fn listed(resolver: &TokioResolver, name: &str, state: &StateStore) -> bool {
    // A Redis cache absorbs the repeated lookups for the same peer or domain;
    // hits live an hour, misses five minutes.
    let key = format!("dnsbl:{name}");
    if let Some(cached) = state.cache_get(&key).await {
        return cached == "1";
    }
    let hit = match resolver.lookup_ip(name).await {
        Ok(lookup) => lookup.iter().next().is_some(),
        Err(_) => return false,
    };
    state
        .cache_set(
            &key,
            if hit { "1" } else { "0" },
            if hit { 3600 } else { 300 },
        )
        .await;
    hit
}

fn reverse_ip(ip: IpAddr, zone: &str) -> Option<String> {
    match ip {
        IpAddr::V4(v4) => {
            let octets = v4.octets();
            Some(format!(
                "{}.{}.{}.{}.{}",
                octets[3], octets[2], octets[1], octets[0], zone
            ))
        }
        IpAddr::V6(_) => None,
    }
}

fn zone_tag(zone: &str) -> String {
    zone.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reverses_ipv4() {
        let ip: IpAddr = "203.0.113.7".parse().unwrap();
        assert_eq!(
            reverse_ip(ip, "zen.spamhaus.org").unwrap(),
            "7.113.0.203.zen.spamhaus.org"
        );
    }

    #[test]
    fn tag_names_are_sanitized() {
        assert_eq!(zone_tag("zen.spamhaus.org"), "ZEN_SPAMHAUS_ORG");
    }
}
