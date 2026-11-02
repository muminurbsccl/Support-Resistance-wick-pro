//! CSV bars for offline backtests, e.g. a TradingView "Export chart data" file.
//! Columns: time, open, high, low, close (extra columns are ignored).
//! `time` may be Unix seconds, Unix milliseconds, RFC 3339, or
//! `YYYY-MM-DD HH:MM[:SS]` (read as UTC).

use crate::bar::Bar;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, NaiveDateTime};
use std::path::Path;

pub fn load(path: &Path) -> Result<Vec<Bar>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut bars = Vec::new();
    let mut cols = [0usize, 1, 2, 3, 4];
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line
            .split([',', ';', '\t'])
            .map(|s| s.trim().trim_matches('"'))
            .collect();
        if n == 0 && f.iter().any(|c| c.eq_ignore_ascii_case("close")) {
            let find = |name: &str| f.iter().position(|c| c.eq_ignore_ascii_case(name));
            cols = [
                find("time")
                    .or_else(|| find("date"))
                    .or_else(|| find("datetime"))
                    .unwrap_or(0),
                find("open").unwrap_or(1),
                find("high").unwrap_or(2),
                find("low").unwrap_or(3),
                find("close").unwrap_or(4),
            ];
            continue;
        }
        let get = |i: usize| f.get(cols[i]).copied().unwrap_or("");
        let num = |i: usize| {
            get(i)
                .parse::<f64>()
                .with_context(|| format!("line {}: bad number '{}'", n + 1, get(i)))
        };
        let time =
            parse_time(get(0)).with_context(|| format!("line {}: bad time '{}'", n + 1, get(0)))?;
        bars.push(Bar {
            time,
            open: num(1)?,
            high: num(2)?,
            low: num(3)?,
            close: num(4)?,
        });
    }
    if bars.is_empty() {
        bail!("no bars in {}", path.display());
    }
    bars.sort_by_key(|b| b.time);
    bars.dedup_by_key(|b| b.time);
    Ok(bars)
}

fn parse_time(s: &str) -> Result<i64> {
    if let Ok(v) = s.parse::<f64>() {
        // Seconds until year ~5138, milliseconds beyond.
        return Ok(if v.abs() < 1e11 {
            (v * 1000.0).round() as i64
        } else {
            v.round() as i64
        });
    }
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(t.timestamp_millis());
    }
    for fmt in [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, fmt) {
            return Ok(t.and_utc().timestamp_millis());
        }
    }
    bail!("unrecognised time format")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("1700000000").unwrap(), 1_700_000_000_000);
        assert_eq!(parse_time("1700000000000").unwrap(), 1_700_000_000_000);
        assert_eq!(
            parse_time("2023-11-14T22:13:20Z").unwrap(),
            1_700_000_000_000
        );
        assert_eq!(
            parse_time("2023-11-14 22:13:20").unwrap(),
            1_700_000_000_000
        );
    }
}
