//! Provider destination rules, shared by configuration and every outbound caller.
use crate::WalletError;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use url::{Host, Url};

pub const DESTINATION_POLICY_KEY: &str = "destinationPolicy";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderNetwork {
    #[default]
    Public,
    Internal,
}

/// Internal permission is for exact addresses at one configured endpoint. It
/// is never a blanket permission to reach the deployment's private network.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderDestinationPolicy {
    pub network: ProviderNetwork,
    #[serde(default)]
    pub allowed_ips: Vec<IpAddr>,
}

fn invalid() -> WalletError {
    WalletError::InvalidInput("provider destination is not permitted by its network policy".into())
}

impl ProviderDestinationPolicy {
    /// Legacy configurations receive the restrictive public default. Reading
    /// malformed persisted policy fails closed instead of widening authority.
    pub fn from_settings(settings: &serde_json::Value) -> Result<Self, WalletError> {
        let policy: Self = match settings.get(DESTINATION_POLICY_KEY) {
            None => Self::default(),
            Some(value) => serde_json::from_value(value.clone()).map_err(|_| invalid())?,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<(), WalletError> {
        match self.network {
            ProviderNetwork::Public if !self.allowed_ips.is_empty() => return Err(invalid()),
            ProviderNetwork::Internal
                if self.allowed_ips.is_empty() || self.allowed_ips.len() > 16 =>
            {
                return Err(invalid());
            }
            _ => {}
        }
        let mut seen = std::collections::HashSet::new();
        if self
            .allowed_ips
            .iter()
            .any(|ip| !internal_address(*ip) || !seen.insert(*ip))
        {
            return Err(invalid());
        }
        Ok(())
    }

    /// Validate syntax and literal addresses without DNS or a provider call.
    pub fn endpoint(&self, base: &str) -> Result<Url, WalletError> {
        self.validate()?;
        if base.len() > 2048 || base.chars().any(char::is_control) {
            return Err(invalid());
        }
        let url = Url::parse(base).map_err(|_| invalid())?;
        if !matches!(url.scheme(), "http" | "https")
            || (self.network == ProviderNetwork::Public && url.scheme() != "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default().is_none_or(|p| p == 0)
        {
            return Err(invalid());
        }
        match url.host().ok_or_else(invalid)? {
            Host::Ipv4(ip) => self.check_addresses(&[ip.into()])?,
            Host::Ipv6(ip) => self.check_addresses(&[ip.into()])?,
            Host::Domain(host) => {
                let host = host.trim_end_matches('.').to_ascii_lowercase();
                // These names have infrastructure semantics regardless of DNS.
                if host.is_empty()
                    || host == "metadata.google.internal"
                    || host == "instance-data.ec2.internal"
                    || (self.network == ProviderNetwork::Public
                        && (host == "localhost"
                            || host.ends_with(".localhost")
                            || host.ends_with(".local")
                            || host.ends_with(".internal")
                            || !host.contains('.')))
                {
                    return Err(invalid());
                }
            }
        }
        Ok(url)
    }

    /// Check the complete DNS answer set. The transport must use exactly this
    /// set, without another resolution or an environment proxy.
    pub fn check_addresses(&self, addresses: &[IpAddr]) -> Result<(), WalletError> {
        if addresses.is_empty()
            || addresses.len() > 16
            || addresses.iter().any(|ip| match self.network {
                ProviderNetwork::Public => !public_address(*ip),
                ProviderNetwork::Internal => {
                    !internal_address(*ip) || !self.allowed_ips.contains(ip)
                }
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip != Ipv4Addr::new(168, 63, 129, 16)
        && !matches!(a, 0 | 10 | 127 | 224..=255)
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
        && !(a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100)))
        && !(a == 203 && b == 0 && c == 113)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // Global unicast only. Exclude transition/documentation/special-purpose
    // ranges, including mapped/compatible/NAT64/6to4 addresses.
    (s[0] & 0xe000) == 0x2000
        && s[0] != 0x2002
        && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
        && !(s[0] == 0x3fff && (s[1] & 0xf000) == 0)
}

pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}

fn internal_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            // Azure's platform endpoint must not become a provider even with
            // an internal grant. Link-local metadata is excluded below too.
            if ip == Ipv4Addr::new(168, 63, 129, 16) {
                return false;
            }
            public_v4(ip) || ip.is_private() || ip.is_loopback()
        }
        IpAddr::V6(ip) => {
            ip != Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
                && (public_v6(ip) || ip.is_loopback() || ip.is_unique_local())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    #[test]
    fn public_policy_refuses_infrastructure_and_ambiguous_endpoints() {
        let policy = ProviderDestinationPolicy::default();
        for ip in [
            "0.0.0.0",
            "127.0.0.1",
            "10.0.0.1",
            "172.31.255.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.100.100.200",
            "168.63.129.16",
            "192.0.2.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "64:ff9b::7f00:1",
            "fe80::1",
            "fd00:ec2::254",
            "2001:db8::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(policy.check_addresses(&[ip]).is_err(), "{ip}");
        }
        for ip in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
            assert!(policy.check_addresses(&[ip.parse().unwrap()]).is_ok());
        }
        for endpoint in [
            "http://api.example/v1",
            "file:///etc/passwd",
            "https://user:secret@api.example/v1",
            "https://api.example/v1?key=secret",
            "https://api.example/v1#fragment",
            "https://localhost/v1",
            "https://localhost./v1",
            "https://metadata.google.internal/",
            "https://api.example:0/v1",
            "https://2130706433/",
            "https://0x7f000001/",
        ] {
            assert!(policy.endpoint(endpoint).is_err(), "{endpoint}");
        }
        assert!(policy.check_addresses(&[]).is_err());
        assert!(
            policy
                .check_addresses(&["8.8.8.8".parse().unwrap(), "127.0.0.1".parse().unwrap()])
                .is_err()
        );
        assert!(policy.endpoint("https://api.example/v1").is_ok());
    }
    #[test]
    fn internal_approval_is_exact_and_never_includes_metadata() {
        let policy = ProviderDestinationPolicy {
            network: ProviderNetwork::Internal,
            allowed_ips: vec!["127.0.0.1".parse().unwrap(), "10.0.0.2".parse().unwrap()],
        };
        assert!(policy.endpoint("http://127.0.0.1:8080/v1").is_ok());
        assert!(
            policy
                .check_addresses(&["10.0.0.3".parse().unwrap()])
                .is_err()
        );
        for ip in [
            "169.254.169.254",
            "168.63.129.16",
            "fd00:ec2::254",
            "::ffff:10.0.0.1",
            "0.0.0.0",
            "fe80::1",
        ] {
            let policy = ProviderDestinationPolicy {
                network: ProviderNetwork::Internal,
                allowed_ips: vec![ip.parse().unwrap()],
            };
            assert!(policy.validate().is_err(), "{ip}");
        }
        assert!(
            ProviderDestinationPolicy::from_settings(
                &serde_json::json!({"destinationPolicy":null})
            )
            .is_err()
        );
    }
}
