//! Configuration: `config.toml` (optional) + environment / `.env`.
//!
//! Strategy defaults are the backtest-tuned "v11" settings of the Pine script
//! (`gold_secret_inside_bar_fakeout_wick_strategy.pine`) and of the frozen
//! Python model (`backtest/config_v11.json`), so all three agree out of the box.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub feed: FeedConfig,
    pub telegram: TelegramConfig,
    pub signals: SignalConfig,
    pub backtest: BacktestConfig,
    pub strategy: Params,
}

// ───────────────────────────── Feed ─────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct FeedConfig {
    /// Chart timeframe in minutes. The strategy was tuned on 5m.
    pub timeframe_min: u32,
    /// Bars loaded at start-up to warm up the zones (TradingView serves ~5,700 5m bars).
    pub history_bars: u32,
    /// Seconds after a bar's close time before it counts as confirmed.
    pub close_grace_secs: u64,
    pub tradingview: TradingViewConfig,
}

impl Default for FeedConfig {
    fn default() -> Self {
        Self {
            timeframe_min: 5,
            history_bars: 5000,
            close_grace_secs: 3,
            tradingview: TradingViewConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TradingViewConfig {
    /// Same symbol as your TradingView chart.
    pub symbol: String,
    /// Optional TradingView session token (prefer TV_AUTH_TOKEN); not needed for OANDA:XAUUSD.
    pub auth_token: String,
    pub ws_url: String,
}

impl Default for TradingViewConfig {
    fn default() -> Self {
        Self {
            symbol: "OANDA:XAUUSD".into(),
            auth_token: String::new(),
            ws_url: "wss://data.tradingview.com/socket.io/websocket?from=chart%2F&type=chart"
                .into(),
        }
    }
}

// ───────────────────────────── Telegram ─────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct TelegramConfig {
    pub enabled: bool,
    /// Prefer the TELEGRAM_BOT_TOKEN environment variable (same as GoldCatcher).
    pub bot_token: String,
    /// Channel / group id, e.g. -1001234567890. Prefer TELEGRAM_CHAT_ID.
    pub chat_id: String,
    /// Optional forum-topic id inside a group.
    pub message_thread_id: Option<i64>,
    /// Post a short "bot online" message at start-up.
    pub notify_startup: bool,
    pub disable_notification: bool,
    /// Bot API base URL (change only for a self-hosted Bot API server).
    pub api_base: String,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            bot_token: String::new(),
            chat_id: String::new(),
            message_thread_id: None,
            notify_startup: true,
            disable_notification: false,
            api_base: "https://api.telegram.org".into(),
        }
    }
}

// ───────────────────────────── Signals ─────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SignalConfig {
    /// Lot size shown in messages and used for $ figures in backtests.
    pub lots: f64,
    /// Units per lot (standard gold lot = 100 oz).
    pub oz_per_lot: f64,
    pub price_decimals: usize,
    /// Time zone used in messages.
    pub display_tz: String,
    pub display_tz_label: String,
    pub ticker: String,
    /// Optional first line of every message, e.g. "#gold".
    pub prefix: String,
    /// Also post fills, cancellations, TP1/TP2, stop and time-stop updates.
    pub updates: bool,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            lots: 0.1,
            oz_per_lot: 100.0,
            price_decimals: 2,
            display_tz: "Asia/Dhaka".into(),
            display_tz_label: "BDT".into(),
            ticker: "XAUUSD".into(),
            prefix: String::new(),
            updates: true,
        }
    }
}

// ───────────────────────────── Backtest ─────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BacktestConfig {
    /// Cost charged on every fill, in price points (≈ spread). 0.25 = 25 cents on gold.
    pub spread: f64,
    /// Folder for downloaded Dukascopy data.
    pub cache_dir: String,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            spread: 0.25,
            cache_dir: "cache".into(),
        }
    }
}

// ───────────────────────────── Strategy ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IbColor {
    Any,
    Colored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LevelMode {
    /// Only true fakeout wicks: the swing swept a prior swing and rejected.
    Sweep,
    SwingOrSweep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfirmMode {
    None,
    Color,
    Wick,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryMode {
    /// Enter at the confirmation candle's close.
    Market,
    /// Limit order at `limit_frac` of the confirmation candle, valid `entry_valid_bars`.
    Limit,
}

/// Strategy inputs. Names follow the Pine script / Python model.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Params {
    /// Symbol tick size (OANDA:XAUUSD = 0.001).
    pub tick: f64,
    // 1) Inside bar & fakeout detection
    pub ib_tol_ticks: f64,
    pub ib_tol_atr: f64,
    pub ib_color: IbColor,
    pub swing_lookback: usize,
    pub ctx_bars: usize,
    pub sweep_wick_pct: f64,
    pub min_sweep_atr: f64,
    pub level_mode: LevelMode,
    // 2) Zone sizing
    pub atr_len: usize,
    pub min_zone_atr: f64,
    pub max_zone_atr: f64,
    // 3) Break / impulse verification
    pub max_break_bars: usize,
    pub min_impulse_mult: f64,
    pub max_impulse_bars: usize,
    // 4) Retest & confirmation
    pub confirm_mode: ConfirmMode,
    pub rej_wick_pct: f64,
    pub confirm_close_outside: bool,
    pub allow_deep_sweep: bool,
    pub min_retest_depth: f64,
    pub confirm_window: usize,
    pub max_zone_age: usize,
    // 4b) Entry
    pub entry_mode: EntryMode,
    pub limit_frac: f64,
    pub entry_valid_bars: usize,
    // 5) Risk
    pub sl_buffer_ticks: f64,
    pub tp1_rr: f64,
    /// Fraction of the position closed at TP1 (0.5 = 50%).
    pub tp1_qty: f64,
    pub tp2_rr: f64,
    pub move_to_be: bool,
    pub be_lock_r: f64,
    /// Close at market if TP1 is not reached within N bars after the fill (0 = off).
    pub time_stop_bars: usize,
    pub min_risk_atr: f64,
    pub max_risk_atr: f64,
    // 6) Filters
    pub use_session: bool,
    /// New York time, HH:MM.
    pub session_start: String,
    pub session_end: String,
    /// 0 = unlimited. The trading day starts 17:00 New York.
    pub max_trades_day: u32,
    pub max_zones: usize,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            tick: 0.001,
            ib_tol_ticks: 0.0,
            ib_tol_atr: 0.02,
            ib_color: IbColor::Any,
            swing_lookback: 30,
            ctx_bars: 3,
            sweep_wick_pct: 0.40,
            min_sweep_atr: 0.0,
            level_mode: LevelMode::Sweep,
            atr_len: 14,
            min_zone_atr: 0.15,
            max_zone_atr: 1.0,
            max_break_bars: 5,
            min_impulse_mult: 0.5,
            max_impulse_bars: 5,
            confirm_mode: ConfirmMode::Wick,
            rej_wick_pct: 0.45,
            confirm_close_outside: true,
            allow_deep_sweep: true,
            min_retest_depth: 0.0,
            confirm_window: 3,
            max_zone_age: 200,
            entry_mode: EntryMode::Limit,
            limit_frac: 0.5,
            entry_valid_bars: 3,
            sl_buffer_ticks: 10.0,
            tp1_rr: 1.5,
            tp1_qty: 0.5,
            tp2_rr: 4.0,
            move_to_be: true,
            be_lock_r: 0.0,
            time_stop_bars: 12,
            min_risk_atr: 0.30,
            max_risk_atr: 2.0,
            use_session: true,
            session_start: "03:00".into(),
            session_end: "16:00".into(),
            max_trades_day: 1,
            max_zones: 50,
        }
    }
}

/// "HH:MM" → minutes after midnight.
pub fn parse_hhmm(s: &str) -> Result<u32> {
    let (h, m) = s
        .trim()
        .split_once(':')
        .with_context(|| format!("expected HH:MM, got '{s}'"))?;
    let (h, m): (u32, u32) = (h.parse()?, m.parse()?);
    if h > 23 || m > 59 {
        bail!("expected HH:MM, got '{s}'");
    }
    Ok(h * 60 + m)
}

// ───────────────────────────── Loading ─────────────────────────────

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let mut cfg: Config = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?
        } else {
            tracing::info!("{} not found – using built-in defaults", path.display());
            Config::default()
        };
        cfg.apply_env();
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        if let Some(v) = env("TELEGRAM_BOT_TOKEN") {
            self.telegram.bot_token = v;
        }
        if let Some(v) = env("TELEGRAM_CHAT_ID") {
            self.telegram.chat_id = v;
        }
        if let Some(v) = env("TV_AUTH_TOKEN") {
            self.feed.tradingview.auth_token = v;
        }
    }

    pub fn validate(&self) -> Result<()> {
        let tf = self.feed.timeframe_min;
        if tf == 0 || 1440 % tf != 0 {
            bail!("feed.timeframe_min must divide a day evenly (1, 5, 15 …), got {tf}");
        }
        let p = &self.strategy;
        if p.atr_len == 0 || p.swing_lookback == 0 || p.ctx_bars == 0 {
            bail!("strategy.atr_len, swing_lookback and ctx_bars must be ≥ 1");
        }
        if !(0.0..1.0).contains(&p.tp1_qty) {
            bail!("strategy.tp1_qty must be between 0 and 1 (0.5 = 50%)");
        }
        if p.tp2_rr < p.tp1_rr {
            bail!("strategy.tp2_rr must be ≥ tp1_rr");
        }
        parse_hhmm(&p.session_start).context("strategy.session_start")?;
        parse_hhmm(&p.session_end).context("strategy.session_end")?;
        self.signals
            .display_tz
            .parse::<chrono_tz::Tz>()
            .map_err(|e| anyhow::anyhow!("signals.display_tz: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_matches_defaults() {
        let cfg: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        cfg.validate().unwrap();
        let d = Params::default();
        assert_eq!(cfg.strategy.swing_lookback, d.swing_lookback);
        assert_eq!(cfg.strategy.tp2_rr, d.tp2_rr);
        assert_eq!(cfg.strategy.entry_mode, EntryMode::Limit);
        assert_eq!(cfg.feed.tradingview.symbol, "OANDA:XAUUSD");
    }

    #[test]
    fn hhmm() {
        assert_eq!(parse_hhmm("03:00").unwrap(), 180);
        assert_eq!(parse_hhmm("16:00").unwrap(), 960);
        assert!(parse_hhmm("24:00").is_err());
    }
}
