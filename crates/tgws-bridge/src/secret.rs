//! Proxy secret parsing.

use crate::error::Error;

/// Parses an MTProto proxy secret: 32 hex digits, optionally prefixed with
/// `dd` (`dd<secret>`, "secure padded" mode) or `ee` (`ee<secret>`).
///
/// # Examples
///
/// ```
/// use tgws_bridge::parse_secret;
///
/// let plain = parse_secret("e00a3136e590b94046bb639c875ec14a").unwrap();
/// let prefixed = parse_secret("dde00a3136e590b94046bb639c875ec14a").unwrap();
/// assert_eq!(plain, prefixed);
/// ```
pub fn parse_secret(raw: &str) -> Result<[u8; 16], Error> {
    let raw = raw.trim();
    let hex = if raw.len() == 34 && (raw.starts_with("dd") || raw.starts_with("ee")) {
        &raw[2..]
    } else {
        raw
    };

    if hex.len() != 32 {
        return Err(Error::Secret(format!(
            "expected 32 hex digits, got {} in «{raw}»",
            hex.len()
        )));
    }

    let mut secret = [0u8; 16];
    for (i, byte) in secret.iter_mut().enumerate() {
        let pair = hex
            .get(i * 2..i * 2 + 2)
            .ok_or_else(|| Error::Secret(format!("«{raw}» is not valid ASCII")))?;
        *byte = u8::from_str_radix(pair, 16)
            .map_err(|_| Error::Secret(format!("«{raw}» contains non-hex characters")))?;
    }
    Ok(secret)
}

#[cfg(test)]
mod tests {
    use super::parse_secret;

    #[test]
    fn plain_and_prefixed_match() {
        let plain = parse_secret("e00a3136e590b94046bb639c875ec14a").unwrap();
        let dd = parse_secret("dde00a3136e590b94046bb639c875ec14a").unwrap();
        let ee = parse_secret("eee00a3136e590b94046bb639c875ec14a").unwrap();
        assert_eq!(plain, dd);
        assert_eq!(plain, ee);
        assert_eq!(plain[0], 0xe0);
        assert_eq!(plain[15], 0x4a);
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            parse_secret("  e00a3136e590b94046bb639c875ec14a\n").unwrap(),
            parse_secret("e00a3136e590b94046bb639c875ec14a").unwrap()
        );
    }

    #[test]
    fn rejects_wrong_input() {
        assert!(parse_secret("").is_err());
        assert!(parse_secret("e00a").is_err());
        assert!(parse_secret("dde00a3136e590b94046bb639c875ec14").is_err());
        assert!(parse_secret("zz0a3136e590b94046bb639c875ec14a").is_err());
        // 33 characters: neither plain hex nor a recognised prefix.
        assert!(parse_secret("de00a3136e590b94046bb639c875ec14a").is_err());
    }

    #[test]
    fn errors_are_displayed() {
        let err = parse_secret("nope").unwrap_err();
        assert!(err.to_string().starts_with("bad secret:"));
    }
}