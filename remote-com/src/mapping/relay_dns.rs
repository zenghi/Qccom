use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    pin::Pin,
    time::Duration,
};

use iroh::dns::{BoxIter, DnsError, DnsResolver, Resolver, TxtRecordData, DNS_TIMEOUT};
use tokio::{net::lookup_host, time::timeout};

type Lookup<T> = Pin<Box<dyn Future<Output = Result<BoxIter<T>, DnsError>> + Send>>;

#[derive(Debug)]
struct SystemResolver {
    fallback: DnsResolver,
}

impl SystemResolver {
    fn new() -> Self {
        Self { fallback: DnsResolver::new() }
    }
}

// Leave part of Iroh's DNS budget for its resolver if the OS cannot resolve
// the hostname. Using the original URL preserves the TLS server name.
async fn system_ips(host: &str) -> Option<Vec<IpAddr>> {
    let addrs = timeout(Duration::from_millis(1200), lookup_host((host, 0))).await.ok()?.ok()?;
    Some(addrs.map(|addr| addr.ip()).collect())
}

impl Resolver for SystemResolver {
    fn lookup_ipv4(&self, host: String) -> Lookup<Ipv4Addr> {
        let fallback = self.fallback.clone();
        Box::pin(async move {
            let ips: BoxIter<IpAddr> = match system_ips(&host).await {
                Some(ips) => Box::new(ips.into_iter()),
                None => Box::new(fallback.lookup_ipv4(host, DNS_TIMEOUT).await?),
            };
            let ipv4 = ips.filter_map(|ip| match ip {
                IpAddr::V4(ip) => Some(ip),
                IpAddr::V6(_) => None,
            });
            Ok(Box::new(ipv4) as BoxIter<_>)
        })
    }

    fn lookup_ipv6(&self, host: String) -> Lookup<Ipv6Addr> {
        let fallback = self.fallback.clone();
        Box::pin(async move {
            let ips: BoxIter<IpAddr> = match system_ips(&host).await {
                Some(ips) => Box::new(ips.into_iter()),
                None => Box::new(fallback.lookup_ipv6(host, DNS_TIMEOUT).await?),
            };
            let ipv6 = ips.filter_map(|ip| match ip {
                IpAddr::V6(ip) => Some(ip),
                IpAddr::V4(_) => None,
            });
            Ok(Box::new(ipv6) as BoxIter<_>)
        })
    }

    fn lookup_txt(&self, host: String) -> Lookup<TxtRecordData> {
        let fallback = self.fallback.clone();
        Box::pin(async move {
            let records = fallback.lookup_txt(host, DNS_TIMEOUT).await?.collect::<Vec<_>>();
            Ok(Box::new(records.into_iter()) as BoxIter<_>)
        })
    }

    fn clear_cache(&self) {
        self.fallback.clear_cache();
    }

    fn reset(&self) -> Box<dyn Resolver> {
        Box::new(Self::new())
    }
}

pub(super) fn relay_dns() -> DnsResolver {
    DnsResolver::custom(SystemResolver::new())
}
