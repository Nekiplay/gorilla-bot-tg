//! Tests that talk to a real `TgWsProxy` instance.
//!
//! They are ignored by default because they need a running proxy:
//!
//! ```text
//! $env:TGWSPROXY_ADDR = "127.0.0.1:1443"
//! $env:TGWSPROXY_SECRET = "dd00112233445566778899aabbccddeeff"
//! cargo test --test live -- --ignored --nocapture
//! ```

use std::net::SocketAddr;
use std::time::Duration;

use tgws_bridge::{parse_secret, probe, Bridge, BridgeConfig};

fn settings() -> Option<(SocketAddr, [u8; 16])> {
    let addr = std::env::var("TGWSPROXY_ADDR").ok()?;
    let secret = std::env::var("TGWSPROXY_SECRET").ok()?;
    let addr: SocketAddr = addr.parse().ok()?;
    Some((addr, parse_secret(&secret).ok()?))
}

/// A plaintext `Intermediate` frame: `auth_key_id`, `msg_id`, `req_pq_multi`.
///
/// Clients such as `grammers` hand plaintext intermediate frames to a local
/// proxy, which is why the bridge encrypts them itself.
fn req_pq_plain() -> Vec<u8> {
    let mut payload = Vec::with_capacity(40);
    payload.extend_from_slice(&[0u8; 8]); // auth_key_id
    payload.extend_from_slice(&[0x11; 8]); // msg_id
    payload.extend_from_slice(&20u32.to_le_bytes()); // body length
    payload.extend_from_slice(&0xbe7e_8ef1u32.to_le_bytes()); // req_pq_multi
    payload.extend_from_slice(&[7u8; 16]); // nonce
    let mut frame = (payload.len() as u32).to_le_bytes().to_vec();
    frame.extend_from_slice(&payload);
    frame
}

#[tokio::test]
#[ignore = "needs a running TgWsProxy"]
async fn proxy_is_alive_and_the_secret_is_right() {
    let (addr, secret) = settings().expect("TGWSPROXY_ADDR and TGWSPROXY_SECRET");
    let report = probe(addr, secret).await.expect("probe");
    println!("{report}");
    assert!(report.contains("answered"), "not a single byte came back");
}

#[tokio::test]
#[ignore = "needs a running TgWsProxy"]
async fn bridge_relays_a_tagged_first_frame() {
    let (addr, secret) = settings().expect("TGWSPROXY_ADDR and TGWSPROXY_SECRET");
    let bridge = Bridge::spawn(
        BridgeConfig::new(addr, secret).with_log(|line| println!("[bridge] {line}")),
    )
    .await
    .expect("spawn");

    let mut client = tokio::net::TcpStream::connect(bridge.local_addr())
        .await
        .expect("connect to the local bridge");

    // SOCKS5 handshake, then CONNECT to a DC2 address.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    client.write_all(&[5, 1, 0]).await.unwrap();
    let mut greeting = [0u8; 2];
    client.read_exact(&mut greeting).await.unwrap();
    assert_eq!(greeting, [5, 0]);
    client
        .write_all(&[5, 1, 0, 1, 149, 154, 167, 51, 0x01, 0xbb])
        .await
        .unwrap();
    let mut reply = [0u8; 10];
    client.read_exact(&mut reply).await.unwrap();
    assert_eq!(reply[1], 0, "the bridge refused the destination");

    // A local client (`grammers` and friends) speaks plaintext `Intermediate`
    // with the transport tag once in front; the bridge strips the tag and
    // obfuscates the stream itself.
    let mut payload = vec![0xee, 0xee, 0xee, 0xee];
    payload.extend_from_slice(&req_pq_plain());
    client.write_all(&payload).await.unwrap();

    // Collect one whole frame: the reply may arrive in several TCP segments.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut answer = Vec::new();
    let mut chunk = vec![0u8; 512];
    while answer.len() < 4 {
        let read = tokio::time::timeout_at(deadline, client.read(&mut chunk))
            .await
            .expect("no first byte in 20s")
            .expect("read");
        assert!(read > 0, "the upstream closed the session");
        answer.extend_from_slice(&chunk[..read]);
    }
    // `Intermediate` framing: the length counts everything after it.
    let packet_len = u32::from_le_bytes([answer[0], answer[1], answer[2], answer[3]]) as usize;
    assert!((4..=65536).contains(&packet_len), "not a frame: {answer:02x?}");
    let total = 4 + packet_len;
    while answer.len() < total {
        let read = tokio::time::timeout_at(deadline, client.read(&mut chunk))
            .await
            .expect("frame did not arrive in 20s")
            .expect("read");
        assert!(read > 0, "the upstream closed mid-frame");
        answer.extend_from_slice(&chunk[..read]);
    }
    // The reply is encrypted with a key the bridge generated, so this test can
    // only check that the framing looks like a real MTProto packet.
    println!("got {total} bytes, first: {:02x?}", &answer[..total.min(24)]);

    bridge.shutdown();
}