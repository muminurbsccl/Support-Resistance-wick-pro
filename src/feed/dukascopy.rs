//! Dukascopy historical data (free, no account): XAUUSD 1-minute BID candles,
//! one LZMA-compressed `.bi5` file per day, aggregated here to the chart
//! timeframe. Files of finished days are cached on disk, so a second backtest
//! over the same range needs no download.
//!
//! Record layout (24 bytes, big-endian): seconds from midnight (i32), open,
//! close, low, high (i32, price × 1000), volume (f32).

use crate::bar::Bar;
use anyhow::{Context, Result, bail};
use chrono::{Datelike, NaiveDate, Utc, Weekday};
use futures_util::{StreamExt, stream};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const BASE: &str = "https://datafeed.dukascopy.com/datafeed";
const PRICE_SCALE: f64 = 1000.0;

/// One-minute candle.
#[derive(Debug, Clone, Copy)]
struct M1 {
    time: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
}

fn url(symbol: &str, day: NaiveDate) -> String {
    // Dukascopy months are 0-based.
    format!(
        "{BASE}/{symbol}/{:04}/{:02}/{:02}/BID_candles_min_1.bi5",
        day.year(),
        day.month0(),
        day.day()
    )
}

fn decode(raw: &[u8], day: NaiveDate) -> Result<Vec<M1>> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    lzma_rs::lzma_decompress(&mut std::io::Cursor::new(raw), &mut out)
        .map_err(|e| anyhow::anyhow!("LZMA: {e:?}"))?;
    let base = day
        .and_hms_opt(0, 0, 0)
        .context("date")?
        .and_utc()
        .timestamp_millis();
    let i32_at = |r: &[u8], k: usize| i32::from_be_bytes([r[k], r[k + 1], r[k + 2], r[k + 3]]);
    let mut rows = Vec::with_capacity(out.len() / 24);
    for r in out.as_chunks::<24>().0 {
        let r: &[u8] = r;
        let vol = f32::from_be_bytes([r[20], r[21], r[22], r[23]]);
        if vol <= 0.0 {
            continue;
        }
        let m = M1 {
            time: base + i32_at(r, 0) as i64 * 1000,
            open: i32_at(r, 4) as f64 / PRICE_SCALE,
            close: i32_at(r, 8) as f64 / PRICE_SCALE,
            low: i32_at(r, 12) as f64 / PRICE_SCALE,
            high: i32_at(r, 16) as f64 / PRICE_SCALE,
        };
        if m.high < m.open.max(m.close) || m.low > m.open.min(m.close) {
            continue;
        }
        rows.push(m);
    }
    Ok(rows)
}

async fn fetch_day(
    http: &reqwest::Client,
    symbol: &str,
    day: NaiveDate,
    cache: &Path,
) -> Result<Vec<M1>> {
    let path = cache.join(symbol).join(format!("{day}.bi5"));
    if let Ok(raw) = std::fs::read(&path) {
        return decode(&raw, day);
    }
    let finished = day < Utc::now().date_naive();
    let mut last_err = None;
    let mut wait = 0u64;
    for attempt in 0..10u32 {
        if wait > 0 {
            tokio::time::sleep(Duration::from_secs(wait)).await;
        }
        // Default back-off: 2, 4, 8, 16, 32, 60, 60 … seconds.
        wait = 2u64.pow(attempt + 1).min(60);
        match http.get(url(symbol, day)).send().await {
            // Rate limited / busy: honor Retry-After when the server sends one.
            Ok(resp)
                if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE =>
            {
                if let Some(s) = resp
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<u64>().ok())
                {
                    wait = s.clamp(1, 300);
                }
                if attempt == 0 {
                    eprintln!(
                        "  Dukascopy is rate-limiting ({}), slowing down…",
                        resp.status()
                    );
                }
                last_err = Some(anyhow::anyhow!("HTTP {}", resp.status()));
            }
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                if finished {
                    cache_write(&path, &[]);
                }
                return Ok(Vec::new());
            }
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(raw) => {
                    let rows = decode(&raw, day)?;
                    if finished {
                        cache_write(&path, &raw);
                    }
                    return Ok(rows);
                }
                Err(e) => last_err = Some(anyhow::anyhow!("{e}")),
            },
            Ok(resp) => last_err = Some(anyhow::anyhow!("HTTP {}", resp.status())),
            Err(e) => last_err = Some(anyhow::anyhow!("{e}")),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("unknown error")))
        .with_context(|| format!("downloading Dukascopy {symbol} {day}"))
}

fn cache_write(path: &Path, data: &[u8]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, data);
}

/// Downloads (or reads from cache) `[from, to]` and aggregates to `tf_min` bars.
pub async fn load(
    symbol: &str,
    from: NaiveDate,
    to: NaiveDate,
    tf_min: u32,
    cache_dir: &Path,
) -> Result<Vec<Bar>> {
    if to < from {
        bail!("--to is before --from");
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent("Mozilla/5.0 (goldib backtester)")
        .build()?;
    let days: Vec<NaiveDate> = from
        .iter_days()
        .take_while(|d| *d <= to)
        .filter(|d| d.weekday() != Weekday::Sat)
        .collect();
    let cache: PathBuf = cache_dir.join("dukascopy");
    let total = days.len();
    eprintln!(
        "Dukascopy {symbol}: {total} days {from} → {to} (cached in {})",
        cache.display()
    );
    let mut done = 0usize;
    let mut m1: Vec<M1> = Vec::new();
    let mut results = stream::iter(days.into_iter().map(|d| {
        let http = http.clone();
        let cache = cache.clone();
        async move { fetch_day(&http, symbol, d, &cache).await }
    }))
    .buffered(2);
    while let Some(r) = results.next().await {
        m1.extend(r?);
        done += 1;
        if done.is_multiple_of(25) || done == total {
            eprintln!("  {done}/{total} days");
        }
    }
    Ok(aggregate(&m1, tf_min))
}

/// 1-minute candles → `tf_min` bars labelled by their open time.
fn aggregate(m1: &[M1], tf_min: u32) -> Vec<Bar> {
    let tf = tf_min as i64 * 60_000;
    let mut out: BTreeMap<i64, Bar> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for m in m1 {
        if !seen.insert(m.time) {
            continue;
        }
        let key = m.time.div_euclid(tf) * tf;
        out.entry(key)
            .and_modify(|b| {
                b.high = b.high.max(m.high);
                b.low = b.low.min(m.low);
                b.close = m.close;
            })
            .or_insert(Bar {
                time: key,
                open: m.open,
                high: m.high,
                low: m.low,
                close: m.close,
            });
    }
    out.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_uses_zero_based_month() {
        let d = NaiveDate::from_ymd_opt(2026, 10, 5).unwrap();
        assert!(url("XAUUSD", d).ends_with("/XAUUSD/2026/09/05/BID_candles_min_1.bi5"));
    }

    #[test]
    fn aggregates_minutes() {
        let m = |t: i64, o: f64, h: f64, l: f64, c: f64| M1 {
            time: t * 60_000,
            open: o,
            high: h,
            low: l,
            close: c,
        };
        let bars = aggregate(
            &[
                m(0, 1.0, 2.0, 0.5, 1.5),
                m(4, 1.5, 3.0, 1.0, 2.5),
                m(5, 2.5, 2.6, 2.4, 2.45),
            ],
            5,
        );
        assert_eq!(bars.len(), 2);
        assert_eq!(
            (bars[0].open, bars[0].high, bars[0].low, bars[0].close),
            (1.0, 3.0, 0.5, 2.5)
        );
        assert_eq!(bars[1].time, 300_000);
    }
}
