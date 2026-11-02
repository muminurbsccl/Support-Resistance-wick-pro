//! goldib – Gold Secret Inside-Bar & Fakeout Wick signals for XAUUSD, posted to Telegram.

mod bar;
mod config;
mod engine;
mod feed;
mod report;
mod signals;
mod telegram;

use anyhow::{Context, Result};
use bar::Bar;
use chrono::{Duration as ChronoDuration, NaiveDate, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use config::Config;
use engine::{ClosedTrade, Engine, Event};
use feed::FeedMsg;
use signals::Notifier;
use std::path::{Path, PathBuf};
use telegram::Telegram;
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(
    name = "goldib",
    version,
    about = "Gold Secret Inside-Bar & Fakeout Wick – XAUUSD 5m signals with SL/TP → Telegram"
)]
struct Cli {
    /// Config file (TOML). Optional: built-in defaults are the backtest-tuned settings.
    #[arg(short, long, global = true, default_value = "config.toml")]
    config: PathBuf,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Source {
    /// Dukascopy 1-minute history (any range, downloaded and cached).
    Dukascopy,
    /// TradingView OANDA:XAUUSD – the live feed's own data (about the last 4 weeks).
    Tradingview,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run live: follow OANDA:XAUUSD and post signals to Telegram (default).
    Run {
        /// Print messages to the console instead of posting them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replay history through the strategy and print the statistics.
    Backtest {
        #[arg(long, value_enum, default_value_t = Source::Dukascopy)]
        source: Source,
        /// First day (UTC), e.g. 2025-11-01. Default: one year ago.
        #[arg(long)]
        from: Option<NaiveDate>,
        /// Last day (UTC), inclusive. Default: yesterday.
        #[arg(long)]
        to: Option<NaiveDate>,
        /// Read bars from a CSV file instead (time, open, high, low, close).
        #[arg(long)]
        csv: Option<PathBuf>,
        /// Lot size for $ figures (default: signals.lots).
        #[arg(long)]
        lots: Option<f64>,
        /// Cost per trade in price points (default: backtest.spread).
        #[arg(long)]
        spread: Option<f64>,
        /// Where to write the Markdown report (default: reports/…).
        #[arg(long)]
        report: Option<PathBuf>,
        /// Also write every trade to this CSV file.
        #[arg(long)]
        trades_csv: Option<PathBuf>,
        /// Also save the bars used to this CSV file.
        #[arg(long)]
        bars_csv: Option<PathBuf>,
        /// Print the Telegram messages the bot would have sent for the last N trades.
        #[arg(long)]
        messages: Option<usize>,
    },
    /// Check the Telegram token and chat id without posting anything.
    Check,
    /// Post a test message to the configured Telegram chat.
    TelegramTest,
    /// List chats the bot has seen, to find your channel's chat id.
    ChatId,
    /// Write a commented config.toml with all defaults.
    InitConfig {
        #[arg(long)]
        force: bool,
    },
}

fn main() -> Result<()> {
    let double_clicked = std::env::args_os().len() == 1;
    let res = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(app());
    if let Err(e) = &res {
        eprintln!("\nERROR: {e:#}");
        // A double-clicked .exe closes its console window on exit: keep the error readable.
        if cfg!(windows) && double_clicked {
            eprintln!("\nPress Enter to close this window…");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
    }
    res
}

/// Folder of the running executable (where the release puts .env and config.toml).
fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()?
        .canonicalize()
        .ok()?
        .parent()
        .map(PathBuf::from)
}

/// Loads `.env` from the working directory, else from the executable's folder.
/// `.env.txt` is accepted too: Windows Explorer hides the extension it adds.
fn load_env() -> Option<PathBuf> {
    let mut dirs = vec![std::env::current_dir().ok()?];
    dirs.extend(exe_dir());
    for dir in dirs {
        for name in [".env", ".env.txt"] {
            let p = dir.join(name);
            if p.is_file() && dotenvy::from_path(&p).is_ok() {
                return Some(p);
            }
        }
    }
    None
}

/// `config.toml` (the default) is looked up next to the executable when it is
/// not in the working directory.
fn resolve_config(path: PathBuf) -> PathBuf {
    if path.exists() || path.is_absolute() || path != Path::new("config.toml") {
        return path;
    }
    exe_dir()
        .map(|d| d.join(&path))
        .filter(|p| p.exists())
        .unwrap_or(path)
}

async fn app() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let env_file = load_env();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,hyper=warn,reqwest=warn".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut cli = Cli::parse();
    if let Some(Cmd::InitConfig { force }) = &cli.cmd {
        return init_config(&cli.config, *force);
    }
    cli.config = resolve_config(cli.config);
    match &env_file {
        Some(p) => tracing::info!("loaded {}", p.display()),
        None => tracing::info!("no .env file found (using environment variables only)"),
    }
    let cfg = Config::load(&cli.config)?;
    match cli.cmd.unwrap_or(Cmd::Run { dry_run: false }) {
        Cmd::Run { dry_run } => run(cfg, dry_run).await,
        Cmd::Backtest {
            source,
            from,
            to,
            csv,
            lots,
            spread,
            report,
            trades_csv,
            bars_csv,
            messages,
        } => {
            backtest(
                cfg,
                BtArgs {
                    source,
                    from,
                    to,
                    csv,
                    lots,
                    spread,
                    report,
                    trades_csv,
                    bars_csv,
                    messages,
                },
            )
            .await
        }
        Cmd::Check => {
            let tg = Telegram::new(&cfg.telegram)?;
            let me = tg
                .get_me()
                .await
                .context("Telegram rejected TELEGRAM_BOT_TOKEN")?;
            println!(
                "Bot:  @{} ({})",
                me.get("username").and_then(|v| v.as_str()).unwrap_or("?"),
                me.get("first_name").and_then(|v| v.as_str()).unwrap_or("")
            );
            if tg.chat_id().is_empty() {
                anyhow::bail!("TELEGRAM_CHAT_ID is not set (run `goldib chat-id`)");
            }
            let chat = tg
                .get_chat()
                .await
                .context("the bot cannot see TELEGRAM_CHAT_ID – is it a member/admin there?")?;
            println!(
                "Chat: {} ({}, id {})",
                chat.get("title")
                    .or_else(|| chat.get("username"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?"),
                chat.get("type").and_then(|v| v.as_str()).unwrap_or("?"),
                tg.chat_id()
            );
            println!("OK – nothing was posted.");
            Ok(())
        }
        Cmd::TelegramTest => {
            let mut tg = Telegram::new(&cfg.telegram)?;
            let me = tg.get_me().await?;
            println!(
                "Bot: @{}",
                me.get("username").and_then(|v| v.as_str()).unwrap_or("?")
            );
            tg.send("✅ goldib test message – the Gold IB Fakeout bot can post here.")
                .await?;
            println!("Test message sent to chat {}", tg.chat_id());
            Ok(())
        }
        Cmd::ChatId => {
            let tg = Telegram::new(&cfg.telegram)?;
            let chats = tg.recent_chats().await?;
            if chats.is_empty() {
                println!(
                    "No chats yet. Add the bot to your channel/group as an admin, post any message there,\nthen run this command again."
                );
            }
            for (id, kind, title) in chats {
                println!("{id:>16}  {kind:<11} {title}");
            }
            Ok(())
        }
        Cmd::InitConfig { .. } => unreachable!(),
    }
}

fn init_config(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            path.display()
        );
    }
    std::fs::write(path, include_str!("../config.example.toml"))?;
    println!("Wrote {}", path.display());
    Ok(())
}

// ═══════════════════════════════ LIVE ═══════════════════════════════

async fn run(cfg: Config, dry_run: bool) -> Result<()> {
    let tf_min = cfg.feed.timeframe_min;
    let tf_ms = tf_min as i64 * 60_000;
    if tf_min != 5 {
        tracing::warn!("the strategy was tuned and backtested on 5m; running on {tf_min}m");
    }

    // Telegram sender task: keeps message order and never blocks the engine.
    let (tg_tx, mut tg_rx) = mpsc::channel::<String>(256);
    let posting = cfg.telegram.enabled && !dry_run;
    if posting {
        let mut tg = Telegram::new(&cfg.telegram).context("Telegram is enabled in the config")?;
        if tg.chat_id().is_empty() {
            anyhow::bail!(
                "Telegram chat id missing: set TELEGRAM_CHAT_ID (run `goldib chat-id` to find it)"
            );
        }
        // Fail fast on a wrong token instead of silently dropping every signal later.
        match tg.get_me().await {
            Ok(me) => tracing::info!(
                "Telegram: bot @{} → chat {}",
                me.get("username").and_then(|v| v.as_str()).unwrap_or("?"),
                tg.chat_id()
            ),
            Err(e) if telegram::is_config_error(&e) => {
                return Err(e).context("Telegram rejected the bot token (TELEGRAM_BOT_TOKEN)");
            }
            Err(e) => tracing::warn!("Telegram not reachable yet ({e:#}); will keep retrying"),
        }
        tokio::spawn(async move {
            while let Some(msg) = tg_rx.recv().await {
                if let Err(e) = tg.send(&msg).await {
                    tracing::error!(
                        "Telegram: {e:#} – message NOT delivered. Check TELEGRAM_CHAT_ID and that the bot can post there."
                    );
                }
            }
        });
    } else {
        tracing::warn!(
            "Telegram posting is off (dry run or telegram.enabled = false) – messages are printed only"
        );
        tokio::spawn(async move {
            while let Some(msg) = tg_rx.recv().await {
                println!("\n──── message ────\n{msg}\n─────────────────");
            }
        });
    }

    let (tx, mut rx) = mpsc::channel::<FeedMsg>(1024);
    let _feed = feed::spawn(&cfg.feed, tx);
    tracing::info!(
        "goldib started: TradingView {} {}m",
        cfg.feed.tradingview.symbol,
        tf_min
    );

    let mut engine = Engine::new(cfg.strategy.clone(), 0.0, tf_min);
    let mut notifier = Notifier::new(&cfg.signals, &cfg.strategy, tf_min);
    let mut last_time = i64::MIN;
    let mut warmed = false;

    while let Some(msg) = rx.recv().await {
        let mut batch: Vec<(Bar, Vec<Event>)> = Vec::new();
        let mut history = false;
        match msg {
            FeedMsg::Info {
                description,
                decimals,
            } => {
                tracing::info!(
                    "symbol: {description} ({} price decimals)",
                    decimals.unwrap_or(3)
                );
            }
            FeedMsg::History(bars) => {
                history = true;
                for b in bars {
                    if b.time > last_time {
                        last_time = b.time;
                        let ev = engine.on_bar(b);
                        batch.push((b, ev));
                    }
                }
            }
            FeedMsg::Closed(b) => {
                if b.time > last_time {
                    last_time = b.time;
                    let ev = engine.on_bar(b);
                    tracing::info!(
                        "bar {} closed {:.2} | {} zone(s) watched | {}",
                        fmt_utc(b.time, "%d %b %H:%M UTC"),
                        b.close,
                        engine.active_zones().count(),
                        if engine.in_trade() {
                            "order/position open"
                        } else {
                            "flat"
                        }
                    );
                    batch.push((b, ev));
                }
            }
        }

        if warmed {
            let now = feed::now_ms();
            for (b, ev) in &batch {
                let late = now - (b.time + tf_ms) > 5 * 60_000;
                for (_, m) in notifier.messages(ev) {
                    let m = if late {
                        format!("⏳ Delayed (feed reconnect)\n{m}")
                    } else {
                        m
                    };
                    tracing::info!("message:\n{}", m.trim_end());
                    let _ = tg_tx.send(m).await;
                }
            }
        }

        if history && !warmed {
            warmed = true;
            let price = engine.last_bar().map(|b| b.close).unwrap_or(f64::NAN);
            let zones = engine.active_zones().count();
            tracing::info!(
                "warm-up done ({} bars, last close {price:.2}, {zones} zone(s) watched) – live",
                batch.len()
            );
            if cfg.telegram.notify_startup {
                let p = &cfg.strategy;
                let _ = tg_tx
                    .send(format!(
                        "🤖 Gold IB Fakeout bot online – {} {}m via TradingView {}\nLast close {price:.2} · {zones} zone(s) watched\nSignals: limit entry, SL beyond the fakeout wick, TP1 1:{} (book {:.0}%, SL → entry), TP2 1:{}",
                        cfg.signals.ticker,
                        tf_min,
                        cfg.feed.tradingview.symbol,
                        p.tp1_rr,
                        p.tp1_qty * 100.0,
                        p.tp2_rr
                    ))
                    .await;
            }
        }
    }
    anyhow::bail!("feed channel closed")
}

fn fmt_utc(ms: i64, fmt: &str) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format(fmt)
        .to_string()
}

// ═══════════════════════════════ BACKTEST ═══════════════════════════════

struct BtArgs {
    source: Source,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
    csv: Option<PathBuf>,
    lots: Option<f64>,
    spread: Option<f64>,
    report: Option<PathBuf>,
    trades_csv: Option<PathBuf>,
    bars_csv: Option<PathBuf>,
    messages: Option<usize>,
}

fn in_range(b: &Bar, from: Option<NaiveDate>, to: Option<NaiveDate>) -> bool {
    let d = chrono::DateTime::from_timestamp_millis(b.time)
        .unwrap_or_default()
        .date_naive();
    from.is_none_or(|f| d >= f) && to.is_none_or(|t| d <= t)
}

async fn backtest(cfg: Config, a: BtArgs) -> Result<()> {
    let tf = cfg.feed.timeframe_min;
    let (bars, source, tag): (Vec<Bar>, String, String) = if let Some(path) = &a.csv {
        let bars: Vec<Bar> = feed::csv::load(path)?
            .into_iter()
            .filter(|b| in_range(b, a.from, a.to))
            .collect();
        (bars, path.display().to_string(), "csv".into())
    } else if a.source == Source::Tradingview {
        let mut fc = cfg.feed.clone();
        fc.history_bars = 20_000;
        eprintln!(
            "loading history from TradingView {} …",
            fc.tradingview.symbol
        );
        let bars: Vec<Bar> = feed::load_tradingview_history(&fc)
            .await?
            .into_iter()
            .filter(|b| in_range(b, a.from, a.to))
            .collect();
        (
            bars,
            format!("TradingView {}", fc.tradingview.symbol),
            "oanda".into(),
        )
    } else {
        let today = Utc::now().date_naive();
        let to = a.to.unwrap_or(today - ChronoDuration::days(1));
        let from = a.from.unwrap_or(to - ChronoDuration::days(365));
        let bars =
            feed::dukascopy::load("XAUUSD", from, to, tf, Path::new(&cfg.backtest.cache_dir))
                .await?;
        (bars, "Dukascopy XAUUSD (bid)".into(), "dukascopy".into())
    };
    if bars.is_empty() {
        anyhow::bail!("no bars for the selected range");
    }
    if let Some(path) = &a.bars_csv {
        let mut out = String::from("time,open,high,low,close\n");
        for b in &bars {
            out.push_str(&format!(
                "{},{},{},{},{}\n",
                b.time / 1000,
                b.open,
                b.high,
                b.low,
                b.close
            ));
        }
        std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))?;
    }

    let lots = a.lots.unwrap_or(cfg.signals.lots);
    let spread = a.spread.unwrap_or(cfg.backtest.spread);
    let mut engine = Engine::new(cfg.strategy.clone(), spread, tf);
    let mut sig_cfg = cfg.signals.clone();
    sig_cfg.lots = lots;
    let mut notifier = Notifier::new(&sig_cfg, &cfg.strategy, tf);
    let mut trades: Vec<ClosedTrade> = Vec::new();
    let mut msgs: Vec<(u64, String)> = Vec::new();
    for b in &bars {
        let ev = engine.on_bar(*b);
        if a.messages.is_some() {
            msgs.extend(notifier.messages(&ev));
        }
        for e in ev {
            if let Event::Closed(t) = e {
                trades.push(t);
            }
        }
    }

    let first = bars.first().map(|b| b.time).unwrap_or(0);
    let last = bars.last().map(|b| b.time).unwrap_or(0);
    let md = report::markdown(&report::ReportInput {
        title: "Gold IB Fakeout – backtest".into(),
        source: source.clone(),
        bars: bars.len(),
        first_bar: first,
        last_bar: last,
        trades: &trades,
        diag: &engine.diag,
        lots,
        oz_per_lot: cfg.signals.oz_per_lot,
        spread,
        settings: toml::to_string(&cfg.strategy).unwrap_or_default(),
    });
    println!("{md}");
    let path = a.report.clone().unwrap_or_else(|| {
        PathBuf::from("reports").join(format!(
            "backtest_{tag}_{}_{}.md",
            fmt_utc(first, "%Y-%m-%d"),
            fmt_utc(last, "%Y-%m-%d")
        ))
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::write(&path, &md).with_context(|| format!("writing {}", path.display()))?;
    eprintln!("report written to {}", path.display());
    if let Some(p) = &a.trades_csv {
        std::fs::write(
            p,
            report::trades_csv(&trades, lots * cfg.signals.oz_per_lot),
        )
        .with_context(|| format!("writing {}", p.display()))?;
        eprintln!("trades written to {}", p.display());
    }
    if let Some(n) = a.messages {
        // Messages of the last n signals.
        let mut keep: Vec<u64> = msgs.iter().map(|(id, _)| *id).collect();
        keep.dedup();
        let keep: Vec<u64> = keep.into_iter().rev().take(n).collect();
        println!("\n## Telegram messages the bot would have sent (last {n} signals)\n");
        for (id, m) in msgs.iter().filter(|(id, _)| keep.contains(id)) {
            println!("```\n{m}\n```  <!-- trade {id} -->\n");
        }
    }
    Ok(())
}
