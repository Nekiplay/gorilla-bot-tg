# tgws-bridge

A local SOCKS5 bridge to a [TgWsProxy](https://github.com/Flowseal/tg-ws-proxy)
instance, for MTProto clients such as Telegram Desktop and `grammers`.

TgWsProxy accepts MTProto clients on `127.0.0.1:1443` directly, but some clients
speak a slightly different dialect than the one the proxy expects. This crate
sits in between, fixes the stream and hands the client a plain
`socks5://127.0.0.1:PORT` URL that every MTProto client understands.

```text
MTProto client ──socks5──▶ tgws-bridge ──obfuscated2──▶ TgWsProxy ──TLS/WS──▶ Telegram DC
```

## What it fixes

* **Transport tag.** A client that speaks `Intermediate` to a local proxy
  prefixes its stream with `0xEEEEEEEE` exactly once, right before the first
  frame. TgWsProxy reads those four bytes as a frame length, ends up with a
  gigantic packet, never forwards the frame and stays silent forever. The bridge
  strips the tag before the stream leaves the process.
* **Relay-init echo.** Some proxies answer the obfuscated2 handshake with their
  own 64-byte header, some do not. Instead of guessing how many leading bytes to
  discard, the bridge looks for the first plausible `[u32 len][payload]` boundary
  and starts the client stream there.
* **Half-dead sockets.** A local proxy can accept a TCP connection, relay the
  handshake into a broken upstream and still answer nothing at all — a state in
  which a naive client loops on timeouts. Every attempt gets a short deadline,
  the socket is re-dialled with a fresh obfuscated2 handshake, and the whole
  session is abandoned with an explanatory message once the proxy stays silent
  several times in a row.

## Quick start

```rust,no_run
use tgws_bridge::{Bridge, BridgeConfig, parse_secret};

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let secret = parse_secret("dd00000000000000000000000000000000")?;
let bridge = Bridge::spawn(
    BridgeConfig::new("127.0.0.1:1443".parse()?, secret).with_log(|line| eprintln!("{line}")),
)
.await?;

println!("point your client at {}", bridge.socks_url());
# Ok(())
# }
```

The bridge listens on an ephemeral port; hand `socks_url()` to the client.

## Finding the proxy automatically

`autodetect` reads the config TgWsProxy already keeps in
`%APPDATA%\TgWsProxy\config.json` (override with `TGWSPROXY_CONFIG`), so host,
port and secret usually need no configuration at all.

```rust,no_run
# async fn run() -> Result<(), Box<dyn std::error::Error>> {
if let Some(proxy) = tgws_bridge::autodetect() {
    println!("{}", proxy.describe());
    println!("{:?}", proxy.probe().await);

    let bridge = tgws_bridge::Bridge::spawn(proxy.bridge_config()?).await?;
    println!("socks5://{}", bridge.local_addr());
}
# Ok(())
# }
```

Examples live in `examples/basic.rs` and `examples/autodetect.rs`:

```console
cargo run --example autodetect
cargo run --example basic -- 127.0.0.1:1443 00000000000000000000000000000000
```

## Diagnostics

`probe` sends a real `req_pq_multi` through a fresh obfuscated2 handshake and
reports one of three outcomes:

| outcome | meaning |
| --- | --- |
| `Answered { bytes, elapsed }` | the proxy relays to Telegram and the secret is right |
| `Closed { elapsed }` | the proxy accepted the connection and dropped it — look further upstream |
| `Silent { waited }` | nothing came back; usually a wrong secret or a dead proxy upstream |

`probe_local` answers the cheaper question "is anything listening here, and is
it really SOCKS5?" without touching the secret.

`Bridge::last_reason()` returns the sentence that explains the last failed
session, for example:

```text
proxy 127.0.0.1:1443 accepted the connection but stayed silent 3 times in a row
— its own upstream is dead; check the proxy log
```

## API map

| item | purpose |
| --- | --- |
| `Bridge`, `BridgeConfig` | start, address, last failure, shutdown |
| `LocalProxy`, `autodetect` | host, port and secret of a local TgWsProxy |
| `Probe`, `probe`, `probe_local`, `Outcome`, `LocalKind` | connectivity checks |
| `parse_secret` | accept `dd…`/`ee…`-prefixed or bare 32-hex secrets |
| `generate_client_handshake`, `AesCtr256` | obfuscated2 primitives, if you build your own bridge |
| `strip_transport_tag`, `first_frame_offset` | the two stream fixes on their own |
| `dc_for_ip`, `DC_ADDRESSES` | Telegram DC ↔ address mapping |
| `socks_accept`, `reply` | SOCKS5 server side and RFC 1928 reply codes |

## Limitations

* The client speaks plaintext `Intermediate` to the bridge, exactly like
  `grammers` does when it is pointed at a local MTProto proxy. A client that
  insists on performing obfuscated2 itself will not be understood, because the
  bridge cannot tell its 64-byte handshake from ordinary data.
* Destinations must be Telegram DC addresses; SOCKS5 `CONNECT` to a domain name
  is rejected with `ADDRESS_UNSUPPORTED`, and an unknown IPv4 address with
  `NOT_ALLOWED`.
* Ports 80/443 semantics, UDP `ASSOCIATE` and SOCKS5 authentication are not
  implemented.

## Licence

MIT.