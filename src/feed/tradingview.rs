//! TradingView chart websocket client – the same data the TradingView chart
//! shows for `OANDA:XAUUSD`, without an account.
//!
//! Adapted from GoldCatcher (session-bias-pro-GoldCatcher, same author), trimmed
//! to the single intraday series this strategy needs.
//!
//! Protocol (unofficial, used by the TradingView web app): every packet is
//! framed as `~m~<len>~m~<payload>`; `~h~N` payloads are heartbeats that must
//! be echoed back. We open one chart session for the intraday series and turn
//! `timescale_update` / `du` messages into bars. A bar is confirmed when a newer
//! bar appears or when its close time plus a small grace period has passed
//! (needed before the daily break).

use super::{FeedMsg, now_ms};
use crate::bar::Bar;
use crate::config::FeedConfig;
use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const CS_BARS: &str = "cs_gibbars";
/// Reconnect when nothing (not even a heartbeat) arrived for this long.
const SILENCE_LIMIT: Duration = Duration::from_secs(90);

pub struct TvFeed {
    cfg: FeedConfig,
}

impl TvFeed {
    pub fn new(cfg: &FeedConfig) -> Self {
        Self { cfg: cfg.clone() }
    }

    /// Runs forever, reconnecting with back-off.
    pub async fn run(self, tx: mpsc::Sender<FeedMsg>) {
        let mut backoff = 2u64;
        loop {
            let started = std::time::Instant::now();
            match self.session(&tx).await {
                Ok(()) => tracing::warn!("TradingView connection closed"),
                Err(e) => tracing::warn!("TradingView feed error: {e:#}"),
            }
            if tx.is_closed() {
                return;
            }
            if started.elapsed() > Duration::from_secs(300) {
                backoff = 2;
            }
            tracing::info!("reconnecting to TradingView in {backoff}s");
            tokio::time::sleep(Duration::from_secs(backoff)).await;
            backoff = (backoff * 2).min(60);
        }
    }

    async fn session(&self, tx: &mpsc::Sender<FeedMsg>) -> Result<()> {
        let tf_ms = self.cfg.timeframe_min as i64 * 60_000;
        let grace_ms = self.cfg.close_grace_secs as i64 * 1000;
        let mut ws = connect(&self.cfg.tradingview.ws_url).await?;
        tracing::info!(
            "connected to TradingView, requesting {} {}m",
            self.cfg.tradingview.symbol,
            self.cfg.timeframe_min
        );

        let token = if self.cfg.tradingview.auth_token.is_empty() {
            "unauthorized_user_token".to_string()
        } else {
            self.cfg.tradingview.auth_token.clone()
        };
        let sym = format!(
            "={}",
            json!({ "symbol": self.cfg.tradingview.symbol, "adjustment": "splits" })
        );
        let tf = self.cfg.timeframe_min.to_string();
        for m in [
            json!({"m": "set_auth_token", "p": [token]}),
            json!({"m": "chart_create_session", "p": [CS_BARS, ""]}),
            json!({"m": "resolve_symbol", "p": [CS_BARS, "sds_sym_1", sym]}),
            json!({"m": "create_series", "p": [CS_BARS, "sds_1", "s1", "sds_sym_1", tf, self.cfg.history_bars + 1, ""]}),
        ] {
            send(&mut ws, &m).await?;
        }

        let mut bars: BTreeMap<i64, Bar> = BTreeMap::new();
        let mut bars_done = false;
        let mut live = false;
        let mut last_emitted = i64::MIN;
        let mut decimals = None;
        let mut description = String::new();
        let mut last_rx = std::time::Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(500));

        loop {
            tokio::select! {
                msg = ws.next() => {
                    let Some(msg) = msg else { return Ok(()) };
                    last_rx = std::time::Instant::now();
                    let text = match msg? {
                        Message::Text(t) => t.to_string(),
                        Message::Close(_) => return Ok(()),
                        _ => continue,
                    };
                    for payload in split_packets(&text) {
                        if payload.starts_with("~h~") {
                            ws.send(Message::text(frame(payload))).await?;
                            continue;
                        }
                        let Ok(v) = serde_json::from_str::<Value>(payload) else { continue };
                        let Some(m) = v.get("m").and_then(Value::as_str) else { continue };
                        let p = v.get("p").and_then(Value::as_array).cloned().unwrap_or_default();
                        let cs = p.first().and_then(Value::as_str).unwrap_or("");
                        match m {
                            "timescale_update" | "du" if cs == CS_BARS => {
                                merge_series(&mut bars, p.get(1));
                            }
                            "series_completed" if cs == CS_BARS => bars_done = true,
                            "symbol_resolved" if cs == CS_BARS => {
                                if let Some(info) = p.get(2) {
                                    decimals = info.get("pricescale").and_then(Value::as_f64)
                                        .filter(|s| *s >= 1.0)
                                        .map(|s| s.log10().round() as u32);
                                    description = info.get("description").and_then(Value::as_str).unwrap_or("").to_string();
                                }
                            }
                            "symbol_error" | "series_error" | "critical_error" | "protocol_error" => {
                                bail!("TradingView {m}: {}", serde_json::to_string(&p).unwrap_or_default());
                            }
                            _ => {}
                        }
                    }
                }
                _ = tick.tick() => {
                    if last_rx.elapsed() > SILENCE_LIMIT {
                        bail!("no data from TradingView for {}s", SILENCE_LIMIT.as_secs());
                    }
                }
            }

            // Initial (or post-reconnect) history once the series is loaded.
            if !live && bars_done {
                let now = now_ms();
                let closed: Vec<Bar> = bars
                    .values()
                    .filter(|b| b.time + tf_ms <= now)
                    .copied()
                    .collect();
                if closed.is_empty() {
                    bail!(
                        "TradingView returned no bars for {}",
                        self.cfg.tradingview.symbol
                    );
                }
                last_emitted = closed.last().map(|b| b.time).unwrap_or(i64::MIN);
                tracing::info!("loaded {} closed bars from TradingView", closed.len());
                tx.send(FeedMsg::Info {
                    decimals,
                    description: description.clone(),
                })
                .await?;
                tx.send(FeedMsg::History(closed)).await?;
                live = true;
            }

            if live {
                let ready = ready_bars(&bars, last_emitted, now_ms(), tf_ms + grace_ms);
                for b in ready {
                    last_emitted = b.time;
                    tx.send(FeedMsg::Closed(b)).await?;
                }
                // Keep memory bounded.
                while bars.len() > 600 {
                    bars.pop_first();
                }
            }
        }
    }
}

/// Bars after `last_emitted` that are confirmed: a newer bar exists, or the
/// bar's close time (+ grace) has passed.
fn ready_bars(
    bars: &BTreeMap<i64, Bar>,
    last_emitted: i64,
    now: i64,
    tf_plus_grace: i64,
) -> Vec<Bar> {
    let newest = bars.keys().next_back().copied().unwrap_or(i64::MIN);
    bars.range(last_emitted.saturating_add(1)..)
        .map(|(_, b)| *b)
        .filter(|b| b.time < newest || b.time + tf_plus_grace <= now)
        .collect()
}

/// Merges `{"sds_1": {"s": [{"i": .., "v": [t, o, h, l, c, vol]}]}}` into `map`.
fn merge_series(map: &mut BTreeMap<i64, Bar>, data: Option<&Value>) -> usize {
    let Some(rows) = data
        .and_then(|d| d.get("sds_1"))
        .and_then(|s| s.get("s"))
        .and_then(Value::as_array)
    else {
        return 0;
    };
    let mut n = 0;
    for r in rows {
        let Some(v) = r.get("v").and_then(Value::as_array) else {
            continue;
        };
        let f = |i: usize| v.get(i).and_then(Value::as_f64);
        if let (Some(t), Some(o), Some(h), Some(l), Some(c)) = (f(0), f(1), f(2), f(3), f(4)) {
            let time = (t * 1000.0).round() as i64;
            map.insert(
                time,
                Bar {
                    time,
                    open: o,
                    high: h,
                    low: l,
                    close: c,
                },
            );
            n += 1;
        }
    }
    n
}

fn frame(payload: &str) -> String {
    format!("~m~{}~m~{}", payload.len(), payload)
}

async fn send(ws: &mut Ws, v: &Value) -> Result<()> {
    ws.send(Message::text(frame(&v.to_string()))).await?;
    Ok(())
}

/// Splits `~m~12~m~{...}~m~3~m~~h~1` into payloads. Uses the header
/// positions rather than the length field, so multi-byte text cannot
/// desynchronise the parser.
fn split_packets(s: &str) -> Vec<&str> {
    let mut heads = Vec::new(); // (header start, payload start)
    let bytes = s.as_bytes();
    let mut i = 0;
    while let Some(pos) = s[i..].find("~m~") {
        let start = i + pos;
        let mut j = start + 3;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > start + 3 && s[j..].starts_with("~m~") {
            heads.push((start, j + 3));
            i = j + 3;
        } else {
            i = start + 3;
        }
        if i >= s.len() {
            break;
        }
    }
    let mut out = Vec::with_capacity(heads.len());
    for (k, &(_, body)) in heads.iter().enumerate() {
        let end = heads.get(k + 1).map(|h| h.0).unwrap_or(s.len());
        out.push(&s[body..end]);
    }
    out
}

// ───────────────────────── connection (with HTTP proxy support) ─────────────────────────

async fn connect(url: &str) -> Result<Ws> {
    let mut req = url.into_client_request()?;
    let h = req.headers_mut();
    h.insert("Origin", "https://www.tradingview.com".parse()?);
    h.insert("User-Agent", "Mozilla/5.0 (goldib signal bot)".parse()?);
    let host = req
        .uri()
        .host()
        .ok_or_else(|| anyhow!("bad ws url"))?
        .to_string();
    let port = req.uri().port_u16().unwrap_or(443);

    let fut = async {
        if let Some(proxy) = proxy_for(&host) {
            let tcp = tunnel(&proxy, &host, port).await.context("proxy CONNECT")?;
            let (ws, _) =
                tokio_tungstenite::client_async_tls_with_config(req, tcp, None, None).await?;
            Ok::<_, anyhow::Error>(ws)
        } else {
            let (ws, _) =
                tokio_tungstenite::connect_async_tls_with_config(req, None, false, None).await?;
            Ok(ws)
        }
    };
    tokio::time::timeout(Duration::from_secs(20), fut)
        .await
        .context("connect timeout")?
}

struct Proxy {
    host: String,
    port: u16,
    auth: Option<String>,
}

/// Honors HTTPS_PROXY / ALL_PROXY (http:// only) and NO_PROXY.
fn proxy_for(host: &str) -> Option<Proxy> {
    let get = |k: &str| {
        std::env::var(k)
            .ok()
            .or_else(|| std::env::var(k.to_lowercase()).ok())
            .filter(|v| !v.is_empty())
    };
    if let Some(np) = get("NO_PROXY") {
        for pat in np.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let pat = pat.trim_start_matches("*.").trim_start_matches('.');
            if pat == "*" || host == pat || host.ends_with(&format!(".{pat}")) {
                return None;
            }
        }
    }
    let raw = get("HTTPS_PROXY").or_else(|| get("ALL_PROXY"))?;
    let rest = raw
        .strip_prefix("http://")
        .unwrap_or(&raw)
        .trim_end_matches('/');
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, hp)) => (Some(a.to_string()), hp),
        None => (None, rest),
    };
    let (h, p) = hostport.rsplit_once(':').unwrap_or((hostport, "80"));
    Some(Proxy {
        host: h.to_string(),
        port: p.parse().ok()?,
        auth,
    })
}

async fn tunnel(p: &Proxy, host: &str, port: u16) -> Result<TcpStream> {
    let mut tcp = TcpStream::connect((p.host.as_str(), p.port)).await?;
    let mut req = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some(a) = &p.auth {
        req.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64(a.as_bytes())
        ));
    }
    req.push_str("\r\n");
    tcp.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if tcp.read(&mut byte).await? == 0 || buf.len() > 8192 {
            bail!("proxy closed the connection");
        }
        buf.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&buf);
    let status = head.split_whitespace().nth(1).unwrap_or("");
    if status != "200" {
        bail!("proxy answered: {}", head.lines().next().unwrap_or(""));
    }
    Ok(tcp)
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_packets() {
        let s = "~m~5~m~hello~m~4~m~~h~1~m~2~m~{}";
        assert_eq!(split_packets(s), vec!["hello", "~h~1", "{}"]);
    }

    #[test]
    fn confirms_bars() {
        let b = |t| Bar {
            time: t,
            open: 1.0,
            high: 1.0,
            low: 1.0,
            close: 1.0,
        };
        let m: BTreeMap<i64, Bar> = [(0, b(0)), (900, b(900)), (1800, b(1800))]
            .into_iter()
            .collect();
        // Forming bar (1800) stays open until its close + grace.
        let r: Vec<i64> = ready_bars(&m, i64::MIN, 2000, 903)
            .iter()
            .map(|b| b.time)
            .collect();
        assert_eq!(r, vec![0, 900]);
        let r: Vec<i64> = ready_bars(&m, 900, 2703, 903)
            .iter()
            .map(|b| b.time)
            .collect();
        assert_eq!(r, vec![1800]);
        assert!(ready_bars(&m, 1800, 9999, 903).is_empty());
    }

    #[test]
    fn b64() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64(b"ab"), "YWI=");
    }

    #[test]
    fn merges_rows() {
        let v: Value =
            serde_json::from_str(r#"{"sds_1":{"s":[{"i":0,"v":[1700000000.0,1,2,0.5,1.5,10]}]}}"#)
                .unwrap();
        let mut m = BTreeMap::new();
        assert_eq!(merge_series(&mut m, Some(&v)), 1);
        assert_eq!(m[&1_700_000_000_000].close, 1.5);
    }
}
