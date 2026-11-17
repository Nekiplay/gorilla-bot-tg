//! Starts a bridge to a running `TgWsProxy` and prints the SOCKS5 address.
//!
//! ```text
//! cargo run --example basic -- 127.0.0.1:1443 dd00112233445566778899aabbccddeeff
//! ```

use std::net::SocketAddr;

use tgws_bridge::{parse_secret, Bridge, BridgeConfig, Error};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    let addr: SocketAddr = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:1443".to_string())
        .parse()
        .expect("first argument must be host:port");
    let secret = parse_secret(
        &args
            .next()
            .unwrap_or_else(|| "00000000000000000000000000000000".to_string()),
    )?;

    let bridge = Bridge::spawn(
        BridgeConfig::new(addr, secret).with_log(|line| println!("{line}")),
    )
    .await?;

    println!("SOCKS5 listening on {}", bridge.socks_url());
    println!("point your Telegram client at it, Ctrl+C to stop");

    tokio::signal::ctrl_c().await.ok();
    if let Some(reason) = bridge.last_reason() {
        println!("last upstream complaint: {reason}");
    }
    bridge.shutdown();
    Ok(())
}