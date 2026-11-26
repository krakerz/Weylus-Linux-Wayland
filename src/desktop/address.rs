use std::net::{IpAddr, SocketAddr};

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use pnet_datalink as datalink;

/// The addresses a tablet can reach the server on, the best guess first.
pub fn tablet_addresses(bind: SocketAddr) -> Vec<SocketAddr> {
    if !bind.ip().is_unspecified() {
        return vec![bind];
    }
    // filtering ipv6 unicast requires nightly or more fiddling, lets wait for nightlies to stabilize...
    let ips: Vec<IpAddr> = datalink::interfaces()
        .iter()
        .filter(|iface| iface.is_up() && !iface.is_loopback())
        .flat_map(|iface| iface.ips.iter())
        .filter(|ip| ip.is_ipv4() == bind.is_ipv4())
        .map(|ip| ip.ip())
        .collect();
    if ips.is_empty() {
        return vec![bind];
    }
    ips.into_iter()
        .map(|ip| SocketAddr::new(ip, bind.port()))
        .collect()
}

pub fn tablet_url(addr: SocketAddr, access_code: Option<&str>) -> String {
    match access_code {
        Some(code) => format!(
            "http://{addr}?access_code={}",
            utf8_percent_encode(code, NON_ALPHANUMERIC)
        ),
        None => format!("http://{addr}"),
    }
}

pub fn qr_svg(url: &str) -> Result<String, qrcode::types::QrError> {
    Ok(qrcode::QrCode::new(url)?
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(240, 240)
        .build())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specific_bind_address_returns_itself() {
        let bind: SocketAddr = "192.168.1.5:1701".parse().unwrap();
        assert_eq!(tablet_addresses(bind), vec![bind]);
    }

    #[test]
    fn unspecified_bind_address_keeps_port_and_family() {
        let bind: SocketAddr = "0.0.0.0:1701".parse().unwrap();
        let addrs = tablet_addresses(bind);
        assert!(!addrs.is_empty());
        assert!(addrs.iter().all(|a| a.port() == 1701 && a.is_ipv4()));
    }

    #[test]
    fn url_encodes_the_access_code() {
        let addr: SocketAddr = "192.168.1.5:1701".parse().unwrap();
        assert_eq!(
            tablet_url(addr, Some("a b&")),
            "http://192.168.1.5:1701?access_code=a%20b%26"
        );
        assert_eq!(tablet_url(addr, None), "http://192.168.1.5:1701");
    }

    #[test]
    fn qr_is_svg() {
        assert!(qr_svg("http://192.168.1.5:1701").unwrap().contains("<svg"));
    }
}
