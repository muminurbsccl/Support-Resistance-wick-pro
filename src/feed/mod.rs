//! Price feeds.
//!
//! * `tradingview` – live + recent history of `OANDA:XAUUSD` (your TradingView chart's data).
//! * `dukascopy`   – long history for backtests (free 1-minute candles, cached on disk).
//! * `csv`         – any CSV with time, open, high, low, close (e.g. TradingView "Export chart data").

pub mod csv;
pub mod dukascopy;
pub mod tradingview;

use crate::bar::Bar;
use crate::config::FeedConfig;
use anyhow::Result;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug)]
pub enum FeedMsg {
    /// Symbol metadata (price decimals).
    Info {
        decimals: Option<u32>,
        description: String,
    },
    /// Closed bars loaded on (re)connect, oldest first.
    History(Vec<Bar>),
    /// One newly closed bar.
    Closed(Bar),
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Starts the live TradingView feed in the background. It reconnects on its own.
pub fn spawn(cfg: &FeedConfig, tx: mpsc::Sender<FeedMsg>) -> JoinHandle<()> {
    let f = tradingview::TvFeed::new(cfg);
    tokio::spawn(async move { f.run(tx).await })
}

/// Runs the TradingView feed until its first `History` batch arrives, then stops it.
pub async fn load_tradingview_history(cfg: &FeedConfig) -> Result<Vec<Bar>> {
    let (tx, mut rx) = mpsc::channel(64);
    let handle = spawn(cfg, tx);
    let res = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        while let Some(m) = rx.recv().await {
            if let FeedMsg::History(b) = m {
                return Some(b);
            }
        }
        None
    })
    .await;
    handle.abort();
    match res {
        Ok(Some(b)) => Ok(b),
        Ok(None) => anyhow::bail!("TradingView feed stopped before delivering history"),
        Err(_) => anyhow::bail!("timed out waiting for history from TradingView"),
    }
}
