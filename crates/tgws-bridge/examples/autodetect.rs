//! Finds a locally installed `TgWsProxy`, checks it, and starts a bridge.
//!
//! ```text
//! cargo run --example autodetect
//! ```

use tgws_bridge::autodetect::{
    autodetect, autodetect_in, config_path, LocalProxy,
};
use tgws_bridge::{Bridge, LocalKind};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // An explicit directory wins, then the platform default.
    let proxy = match std::env::args().nth(1) {
        Some(dir) => autodetect_in(std::path::Path::new(&dir)),
        None => autodetect(),
    };

    let Some(proxy) = proxy else {
        println!("no config found (looked at {:?})", config_path());
        println!("falling back to {}:{}", tgws_bridge::DEFAULT_HOST, tgws_bridge::DEFAULT_PORT);
        println!("pass the secret yourself, see the `basic` example");
        return Ok(());
    };

    println!("config:  {}", proxy.describe());
    match proxy.probe().await {
        LocalKind::Socks5 => println!("port:    SOCKS5 proxy is answering"),
        LocalKind::Open => println!("port:    open, but not SOCKS5"),
        LocalKind::Closed => println!("port:    closed, is TgWsProxy running?"),
    }

    let bridge = Bridge::spawn(proxy.bridge_config()?.with_log(|line| println!("{line}"))).await?;
    println!("bridge:  {}", bridge.socks_url());

    // Give the caller something to copy before the process ends.
    let _keepalive = &LocalProxy::defaults(None);
    tokio::signal::ctrl_c().await.ok();
    bridge.shutdown();
    Ok(())
}