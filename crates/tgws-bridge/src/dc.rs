//! Telegram data centre addresses to signed DC indexes.

use std::net::{IpAddr, Ipv4Addr};

/// Known Telegram data centre IPv4 addresses and their signed index.
///
/// A negative index is the media ring of that DC (`-2` is "DC2 media").
/// MTProto clients connect to a bare IP and the proxy has to know which DC
/// the client asked for, so this table is the only mapping available.
pub const DC_ADDRESSES: &[(i16, Ipv4Addr)] = &[
    (1, Ipv4Addr::new(149, 154, 175, 50)),
    (1, Ipv4Addr::new(149, 154, 175, 51)),
    (1, Ipv4Addr::new(149, 154, 175, 53)),
    (1, Ipv4Addr::new(149, 154, 175, 54)),
    (-1, Ipv4Addr::new(149, 154, 175, 52)),
    (2, Ipv4Addr::new(149, 154, 167, 41)),
    (2, Ipv4Addr::new(149, 154, 167, 50)),
    (2, Ipv4Addr::new(149, 154, 167, 51)),
    (2, Ipv4Addr::new(149, 154, 167, 220)),
    (2, Ipv4Addr::new(95, 161, 76, 100)),
    (-2, Ipv4Addr::new(149, 154, 167, 151)),
    (-2, Ipv4Addr::new(149, 154, 167, 222)),
    (-2, Ipv4Addr::new(149, 154, 167, 223)),
    (-2, Ipv4Addr::new(149, 154, 162, 123)),
    (3, Ipv4Addr::new(149, 154, 175, 100)),
    (3, Ipv4Addr::new(149, 154, 175, 101)),
    (-3, Ipv4Addr::new(149, 154, 175, 102)),
    (4, Ipv4Addr::new(149, 154, 167, 91)),
    (4, Ipv4Addr::new(149, 154, 167, 92)),
    (-4, Ipv4Addr::new(149, 154, 164, 250)),
    (-4, Ipv4Addr::new(149, 154, 166, 120)),
    (-4, Ipv4Addr::new(149, 154, 166, 121)),
    (-4, Ipv4Addr::new(149, 154, 167, 118)),
    (-4, Ipv4Addr::new(149, 154, 165, 111)),
    (-4, Ipv4Addr::new(149, 154, 165, 136)),
    (5, Ipv4Addr::new(91, 108, 56, 100)),
    (5, Ipv4Addr::new(91, 108, 56, 101)),
    (5, Ipv4Addr::new(91, 108, 56, 104)),
    (5, Ipv4Addr::new(91, 108, 56, 116)),
    (5, Ipv4Addr::new(91, 108, 56, 126)),
    (5, Ipv4Addr::new(149, 154, 171, 5)),
    (-5, Ipv4Addr::new(91, 108, 56, 102)),
    (-5, Ipv4Addr::new(91, 108, 56, 128)),
    (-5, Ipv4Addr::new(91, 108, 56, 147)),
    (-5, Ipv4Addr::new(91, 108, 56, 151)),
];

/// Maps an IPv4 data centre address to its signed index; `None` for anything
/// that is not in [`DC_ADDRESSES`] (including every IPv6 address).
///
/// # Examples
///
/// ```
/// use std::net::IpAddr;
/// use tgws_bridge::dc_for_ip;
///
/// let ip: IpAddr = "149.154.167.51".parse().unwrap();
/// assert_eq!(dc_for_ip(ip), Some(2));
/// ```
pub fn dc_for_ip(ip: IpAddr) -> Option<i16> {
    match ip {
        IpAddr::V4(v4) => DC_ADDRESSES
            .iter()
            .find(|(_, known)| *known == v4)
            .map(|(dc, _)| *dc),
        IpAddr::V6(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::dc_for_ip;
    use std::net::IpAddr;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn main_and_media_rings() {
        assert_eq!(dc_for_ip(ip("149.154.167.51")), Some(2));
        assert_eq!(dc_for_ip(ip("149.154.167.151")), Some(-2));
        assert_eq!(dc_for_ip(ip("149.154.175.100")), Some(3));
        assert_eq!(dc_for_ip(ip("91.108.56.126")), Some(5));
    }

    #[test]
    fn unknown_addresses() {
        assert_eq!(dc_for_ip(ip("8.8.8.8")), None);
        assert_eq!(dc_for_ip(ip("2a0a:f280::1")), None);
    }
}