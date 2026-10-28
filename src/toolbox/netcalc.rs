//! IP 子网计算：输入 192.168.1.10/24、192.168.1.10 255.255.255.0 或只输入 IP（按类别给默认掩码）。

use std::net::Ipv4Addr;

pub struct Subnet {
    pub ip: Ipv4Addr,
    pub prefix: u8,
}

pub struct Info {
    pub network: Ipv4Addr,
    pub broadcast: Ipv4Addr,
    pub mask: Ipv4Addr,
    pub wildcard: Ipv4Addr,
    pub first: Ipv4Addr,
    pub last: Ipv4Addr,
    /// 可用主机数
    pub hosts: u64,
    pub class: char,
    pub private: bool,
}

fn mask_of(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix as u32)
    }
}

fn prefix_of_mask(m: u32) -> Option<u8> {
    let p = m.leading_ones() as u8;
    (mask_of(p) == m).then_some(p)
}

pub fn parse(s: &str) -> Result<Subnet, String> {
    let s = s.trim();
    let (ip_s, rest) = match s.split_once(['/', ' ', '\t']) {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (s, None),
    };
    let ip: Ipv4Addr = ip_s.parse().map_err(|_| format!("IP: {ip_s}"))?;
    let prefix = match rest.filter(|r| !r.is_empty()) {
        Some(r) if r.contains('.') => {
            let m: Ipv4Addr = r.parse().map_err(|_| format!("mask: {r}"))?;
            prefix_of_mask(u32::from(m)).ok_or_else(|| format!("mask: {r}"))?
        }
        Some(r) => {
            let p: u8 = r.trim_start_matches('/').parse().map_err(|_| format!("/{r}"))?;
            if p > 32 {
                return Err(format!("/{p}"));
            }
            p
        }
        None => match ip.octets()[0] {
            0..=127 => 8,
            128..=191 => 16,
            _ => 24,
        },
    };
    Ok(Subnet { ip, prefix })
}

impl Subnet {
    pub fn info(&self) -> Info {
        let ip = u32::from(self.ip);
        let m = mask_of(self.prefix);
        let net = ip & m;
        let bc = net | !m;
        let (first, last, hosts) = match self.prefix {
            32 => (net, net, 1),
            31 => (net, bc, 2),
            _ => (net + 1, bc - 1, (bc - net - 1) as u64),
        };
        let o = self.ip.octets();
        Info {
            network: net.into(),
            broadcast: bc.into(),
            mask: m.into(),
            wildcard: (!m).into(),
            first: first.into(),
            last: last.into(),
            hosts,
            class: match o[0] {
                0..=127 => 'A',
                128..=191 => 'B',
                192..=223 => 'C',
                224..=239 => 'D',
                _ => 'E',
            },
            private: self.ip.is_private() || self.ip.is_loopback() || self.ip.is_link_local() || (o[0] == 100 && (64..128).contains(&o[1])),
        }
    }

    /// 两个地址是否在同一网段（配置设备 IP 时最常问的）
    pub fn contains(&self, other: Ipv4Addr) -> bool {
        let m = mask_of(self.prefix);
        u32::from(self.ip) & m == u32::from(other) & m
    }
}

pub fn binary(a: Ipv4Addr) -> String {
    a.octets().iter().map(|o| format!("{o:08b}")).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnets() {
        let s = parse("192.168.1.77/26").unwrap();
        let i = s.info();
        assert_eq!(i.network.to_string(), "192.168.1.64");
        assert_eq!(i.broadcast.to_string(), "192.168.1.127");
        assert_eq!((i.first.to_string(), i.last.to_string(), i.hosts), ("192.168.1.65".into(), "192.168.1.126".into(), 62));
        assert!(i.private);
        let s = parse("10.0.0.1 255.255.0.0").unwrap();
        assert_eq!(s.prefix, 16);
        assert!(s.contains("10.0.200.3".parse().unwrap()));
        assert!(parse("10.0.0.1 255.0.255.0").is_err());
        assert_eq!(parse("172.16.5.4").unwrap().prefix, 16);
        assert_eq!(parse("1.2.3.4/32").unwrap().info().hosts, 1);
    }
}
