//! Server side of the SOCKS5 CONNECT handshake.

use std::net::IpAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::dc::dc_for_ip;
use crate::error::{protocol, unsupported, Error};

const VERSION: u8 = 5;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_NONE_ACCEPTABLE: u8 = 0xff;
const CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

/// Reply codes from RFC 1928 §6, for [`crate::Bridge`] integrators that reject
/// connections on their own.
pub mod reply {
    /// Request granted.
    pub const SUCCEEDED: u8 = 0x00;
    /// Connection not allowed by ruleset.
    pub const NOT_ALLOWED: u8 = 0x02;
    /// Host unreachable.
    pub const HOST_UNREACHABLE: u8 = 0x04;
    /// Command not supported.
    pub const COMMAND_UNSUPPORTED: u8 = 0x07;
    /// Address type not supported.
    pub const ADDRESS_UNSUPPORTED: u8 = 0x08;
}

/// Performs the SOCKS5 greeting and CONNECT request, returning the requested
/// data centre index.
///
/// Only `CONNECT` to an IPv4 or IPv6 literal is accepted: the MTProto proxy
/// protocol has no notion of host names, and the destination is used solely to
/// learn which DC the client wants, so a domain name cannot be resolved here.
/// A failure is reported to the client with the matching RFC 1928 reply code
/// before the error is returned.
pub async fn socks_accept(client: &mut TcpStream) -> Result<i16, Error> {
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await?;
    if greeting[0] != VERSION {
        return Err(protocol(format!("socks version {}", greeting[0])));
    }
    let mut methods = vec![0u8; greeting[1] as usize];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&METHOD_NO_AUTH) {
        reply_with(client, METHOD_NONE_ACCEPTABLE).await?;
        return Err(unsupported("client requires SOCKS5 authentication"));
    }
    client.write_all(&[VERSION, METHOD_NO_AUTH]).await?;

    let mut request = [0u8; 4];
    client.read_exact(&mut request).await?;
    if request[0] != VERSION {
        return Err(protocol(format!("socks request version {}", request[0])));
    }
    if request[1] != CMD_CONNECT {
        reply_code(client, reply::COMMAND_UNSUPPORTED).await?;
        return Err(unsupported(format!(
            "socks command {:#04x}: only CONNECT is supported",
            request[1]
        )));
    }
    if request[2] != 0 {
        return Err(protocol(format!("socks reserved byte {:#04x}", request[2])));
    }

    let dst_ip = match request[3] {
        ATYP_IPV4 => {
            let mut bytes = [0u8; 4];
            client.read_exact(&mut bytes).await?;
            IpAddr::from(bytes)
        }
        ATYP_IPV6 => {
            let mut bytes = [0u8; 16];
            client.read_exact(&mut bytes).await?;
            IpAddr::from(bytes)
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            client.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            client.read_exact(&mut name).await?;
            // The port field is still on the wire: drain it, otherwise closing
            // the socket with unread bytes turns the reply into a TCP reset.
            let mut port = [0u8; 2];
            let _ = client.read_exact(&mut port).await;
            reply_code(client, reply::ADDRESS_UNSUPPORTED).await?;
            return Err(unsupported(format!(
                "socks address type domain «{}»: connect by IP instead",
                String::from_utf8_lossy(&name)
            )));
        }
        other => {
            // Nothing else follows in the request, so no drain is needed here.
            reply_code(client, reply::ADDRESS_UNSUPPORTED).await?;
            return Err(unsupported(format!("socks address type {other}")));
        }
    };

    let mut port = [0u8; 2];
    client.read_exact(&mut port).await?;
    let _port = u16::from_be_bytes(port);

    let dc_idx = match dc_for_ip(dst_ip) {
        Some(dc) => dc,
        None => {
            reply_code(client, reply::NOT_ALLOWED).await?;
            return Err(Error::UnknownDc(dst_ip));
        }
    };

    reply_code(client, reply::SUCCEEDED).await?;
    Ok(dc_idx)
}

/// Sends a SOCKS5 reply with an all-zero bound address.
pub(crate) async fn reply_code(client: &mut TcpStream, code: u8) -> Result<(), Error> {
    client
        .write_all(&[VERSION, code, 0, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await?;
    Ok(())
}

async fn reply_with(client: &mut TcpStream, method: u8) -> Result<(), Error> {
    client.write_all(&[VERSION, method]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        let dial = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
        let (server, _) = listener.accept().await.unwrap();
        (server, dial.await.unwrap())
    }

    #[tokio::test]
    async fn accepts_known_dc() {
        let (mut server, mut client) = pair().await;
        let task = tokio::spawn(async move { socks_accept(&mut server).await });

        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0u8; 2];
        client.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, 0]);

        // CONNECT 149.154.167.51:443
        let mut request = vec![5, 1, 0, 1, 149, 154, 167, 51, 0x01, 0xbb];
        client.write_all(&request).await.unwrap();
        let mut answer = [0u8; 10];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer, [5, 0, 0, 1, 0, 0, 0, 0, 0, 0]);

        request.clear();
        assert_eq!(task.await.unwrap().unwrap(), 2);
    }

    #[tokio::test]
    async fn rejects_unknown_dc() {
        let (mut server, mut client) = pair().await;
        let task = tokio::spawn(async move { socks_accept(&mut server).await });

        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0u8; 2];
        client.read_exact(&mut method).await.unwrap();
        client
            .write_all(&[5, 1, 0, 1, 8, 8, 8, 8, 0, 80])
            .await
            .unwrap();
        let mut answer = [0u8; 10];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[1], reply::NOT_ALLOWED);
        assert!(matches!(
            task.await.unwrap(),
            Err(Error::UnknownDc(_))
        ));
    }

    #[tokio::test]
    async fn rejects_domain_destination() {
        let (mut server, mut client) = pair().await;
        let task = tokio::spawn(async move { socks_accept(&mut server).await });

        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut method = [0u8; 2];
        client.read_exact(&mut method).await.unwrap();
        let mut request = vec![5, 1, 0, 3, 4];
        request.extend_from_slice(b"dc2.");
        request.extend_from_slice(&[0x01, 0xbb]);
        client.write_all(&request).await.unwrap();
        let mut answer = [0u8; 10];
        client.read_exact(&mut answer).await.unwrap();
        assert_eq!(answer[1], reply::ADDRESS_UNSUPPORTED);
        assert!(matches!(
            task.await.unwrap(),
            Err(Error::Unsupported(_))
        ));
        request.clear();
    }

    #[tokio::test]
    async fn rejects_authentication() {
        let (mut server, mut client) = pair().await;
        let task = tokio::spawn(async move { socks_accept(&mut server).await });

        client.write_all(&[5, 1, 2]).await.unwrap();
        let mut method = [0u8; 2];
        client.read_exact(&mut method).await.unwrap();
        assert_eq!(method, [5, METHOD_NONE_ACCEPTABLE]);
        assert!(matches!(
            task.await.unwrap(),
            Err(Error::Unsupported(_))
        ));
    }
}