# Gold Secret Inside-Bar & Fakeout Wick Strategy [1:2 / 1:3 RR]

Open-source TradingView **Pine Script® v6 strategy** for the Gold (XAUUSD) 5-minute
inside-bar / fakeout-wick pinpoint entry. No external libraries.

File: `gold_secret_inside_bar_fakeout_wick_strategy.pine`

## 🤖 goldib – Telegram signal bot (Windows · macOS · Linux)

`goldib` is a small Rust program that watches **OANDA:XAUUSD 5m** (the same candles as your
TradingView chart, no account needed), runs this strategy on every closed candle and posts each
signal with **entry, SL, TP1 and TP2** to your Telegram channel, followed by fill, TP, stop and
time-stop updates. It also contains the backtester that produced the results below.

**Same signals as the backtest.** The strategy engine (`src/engine.rs`) is a line-by-line port of
the Python model in `backtest/sim.py`, which was checked trade by trade against TradingView's OANDA
strategy tester. On 12 months of identical bars the Rust engine reproduces the Python model's 73
trades exactly (same entries, stops, targets, exits and R, same signal funnel), and a regression
test (`cargo test`) keeps it that way. The live bot and the backtester share the same engine.

### Setup

1. **Download** the file for your computer from the latest GitHub Actions run or release
   (`goldib-windows-x86_64.exe`, `goldib-macos-arm64`, `goldib-macos-x86_64`, `goldib-linux-x86_64`),
   or build it yourself with `cargo build --release`.
2. **Telegram credentials:** put a `.env` file next to the program. It uses the **same variables as
   GoldCatcher** (session-bias-pro), so you can copy your existing GoldCatcher `.env` unchanged:
   ```
   TELEGRAM_BOT_TOKEN=123456789:AA...
   TELEGRAM_CHAT_ID=-1001234567890
   ```
   The bot must be an admin of the channel (or a member of the group) to post.
3. **Check** the token and chat without posting anything: `goldib check`
4. **Optional:** `goldib telegram-test` posts one test message; `goldib init-config` writes a
   commented `config.toml` (lot size, time zone, message options, all strategy inputs).
5. **Start:** `goldib` (or double-click it on Windows). Use `goldib run --dry-run` to watch the
   messages in the console without posting. Keep it running during the trading week; it reconnects
   on its own and only posts candles that closed after start-up.

### What a signal looks like

```
🔴 SELL LIMIT · XAUUSD 5m
Entry  4158.44
SL     4160.65  (2.21 pts)
TP1    4155.12  1:1.5 · book 50%, SL → entry
TP2    4149.60  1:4
⏳ Valid until 16:15 BDT – cancel if not filled
⏱ Close at market if TP1 is not hit within 60 min
📐 Resistance 4157.87–4160.64 · fakeout wick + inside bar, first retest rejected
💵 0.10 lot: risk $22 · TP1 then BE +$17 · TP1 + TP2 +$61
🕒 Thu 15 Oct 16:00 BDT
```
Follow-ups: `✅ Filled`, `⌛ Not filled – cancelled`, `🎯 TP1 hit – 50% booked, SL moved to entry`,
`🏆 TP2 hit`, `➖ Runner stopped at entry`, `🛑 Stop loss hit`, `⏱ Time stop`, each with the
result in R and in $ at your lot size.

### Backtest results (v11 defaults, 0.25-point cost per trade, 0.1 lot)

| Data | Period | Trades | Win rate | Profit factor | Net | Max drawdown |
|---|---|---|---|---|---|---|
| Dukascopy XAUUSD | Nov 2025 – Oct 2026 (12 months) | 67 | 66% | 2.52 (R) · 2.30 ($) | +32.6R · +$1,900 | 8.1R · $674 |
| TradingView OANDA:XAUUSD | 4 Oct – 2 Nov 2026 | 7 | 86% | 6.27 (R) · 7.24 ($) | +5.6R · +$313 | 1.1R · $50 |

10 of 12 months were positive; the longest losing streak was 5 trades. Full reports with monthly
tables and every trade: [`docs/backtest_dukascopy_12m.md`](docs/backtest_dukascopy_12m.md) and
[`docs/backtest_oanda_4w.md`](docs/backtest_oanda_4w.md). Reproduce them with:
```
goldib backtest --source dukascopy --from 2025-11-01 --to 2026-10-30   # long history, cached in ./cache
goldib backtest --source tradingview                                   # OANDA bars, about the last 4 weeks
goldib backtest --source tradingview --messages 5                      # also print the Telegram messages
```
At 0.1 lot on a $1,000 account the average risk per trade is about $66 (6.6%) and the worst
trade lost $129, so a five-loss streak costs roughly a third of the account. Size accordingly.
Past performance is not a guarantee.

### Commands

| Command | What it does |
|---|---|
| `goldib` / `goldib run` | Live signals → Telegram (`--dry-run` prints instead of posting) |
| `goldib backtest` | Backtest (`--source dukascopy\|tradingview`, `--csv FILE`, `--from/--to`, `--lots`, `--spread`, `--trades-csv`, `--messages N`) |
| `goldib check` | Verify bot token and chat id, posts nothing |
| `goldib telegram-test` | Post a test message |
| `goldib chat-id` | List chats the bot has seen, to find the channel id |
| `goldib init-config` | Write a commented `config.toml` |

## 8-step workflow (mirrored on the on-chart checklist dashboard)

| # | Step | Logic |
|---|------|-------|
| 1 | 🔥 LEVEL MARK | Mother Bar (or up to 2 preceding bars) is a local swing high/low (`ta.highest` / `ta.lowest`) or sweeps a prior swing with a rejection wick. |
| 2 | 📊 INSIDE BAR | `high <= high[1] + tol and low >= low[1] - tol`. Optional colored-IB filter. |
| 3 | ⬆️⬇️ BREAK | Close beyond the Mother Bar away from the level within `max_break_bars`, then an impulse of `min_impulse_mult × zone height`. Zone becomes `VERIFIED_ACTIVE`. |
| 4 | 🔁 RETEST | First pullback into the verified box (wick must stay within `sl_buffer` of the outer edge). |
| 5 | ✅ CONFIRMATION | Close back inside/beyond the inner edge plus optional candle-color / rejection-wick filter, within a confirmation window. |
| 6 | 🎯 ENTRY | Long at Support / Short at Resistance. **One trade per zone** (`is_used`). A close beyond the opposite side invalidates the zone. |
| 7 | 🛑 SMALL SL | Just beyond the outer boundary of the box plus a tick or ATR buffer. |
| 8 | 💰 1:2 BOOK / 1:3 FINAL | Partial close (default 50%) at 1:2 RR with optional move to breakeven; remainder at 1:3 RR (configurable up to 1:5). |

## Zone sizing modes

* **Local Fakeout Wick / Inside Bar Zone** (default): box spans the fakeout wick tip to the
  Mother Bar body / Inside Bar edge, clamped between `min_zone_atr` and `max_zone_atr`.
* **Daily Reference Wick Size (Locked Daily Height)**: the wick height of the first verified
  setup after the daily open is locked and reused for every box that day.

## Visuals

* Maroon boxes with a yellow border and centered `support` / `resistance` text for verified zones.
* `IB` circle markers on the Inside Bar, `BREAK` triangles on the breakout candle.
* Green TP box (entry → TP2) with a dashed `1:2 BOOK` line, red SL box, and a measurement label
  (`±points (±%) ±ticks | n bars`).
* Dark-themed 8-step checklist table with live status and prices (corner configurable).

## Backtest-tuned defaults (12 months of Dukascopy XAUUSD 5m, Oct 2025 – Oct 2026)

The logic was ported to Python and every input was swept one at a time, then in
combination, scored on profit factor, expectancy and stability across both halves of
the year and on the 15m timeframe. Defaults were set to a robust plateau, not a single
best point. Settings that **hurt** and are therefore off by default: EMA trend filter,
key-level confluence, minimum retest depth.

| Setting | Default | Why |
|---------|---------|-----|
| Entry mode | Limit pullback at 50% of the confirmation candle, valid 3 bars | Biggest single improvement (PF 1.21 → 1.46 alone). Smaller risk, same price move = more R. |
| Min impulse after break | 0.5 × zone height within 5 bars | Faster verification keeps the freshest zones. |
| Time stop | 12 bars after fill if TP1 not hit | Cuts stale trades before they become full losers. |
| Max trades per day | 1 | The first valid setup of the day is the best one; later ones underperform. |
| Rejection wick | ≥ 45% of the confirmation candle | Stronger rejection, fewer marginal retests. |
| Swing lookback | 30 bars | A 30-bar swing is a more meaningful level than a 20-bar one. |
| Session | 03:00–16:00 New York | 08:00–16:00 gives a higher PF (≈2.6) with ~40% fewer trades. |

Result on 5m with a 0.25-point spread: 73 trades/year, profit factor 2.27, +0.48R per
trade, and above 2.0 in both halves of the year, on 15m, and on the recent two months.
For more trades (≈127/year, PF ≈2.05) set Max trades per day = 2, lookback = 20, rejection
wick = 0.40. The default **Target profile** is 1:1.5 book / 1:4 final (PF ≈2.8, win rate 67%, same
trades and similar net R as the method's 1:2 / 1:3, which is one click away in the inputs).
Loss streaks in the backtest year: max 5 in a row, three streaks of 3 or more. At 0.1 lot on
$1,000 a 5-loss streak is roughly −$300, so size accordingly. Execution was verified trade by
trade against a TradingView OANDA run (fills, TP1/TP2, stops and time stops all matched).

Sizing note: with a fixed 0.1 lot (10 oz) on a $1,000 account the same year nets about
+$3,600 but with a $593 max drawdown and a $154 worst trade. 0.03 lot keeps risk near 2%.
Past performance is not a guarantee; re-validate on your broker's feed.

## Usage

1. Open TradingView, Pine Editor, paste the script, click **Add to chart**.
2. Recommended: XAUUSD, 5-minute chart. Tune the ATR clamps and buffers for other symbols.

Drawings are capped (`max_zones`, `max_trade_viz`) so `max_boxes_count = 500` is never exceeded.

The time stop counts from the candle the limit order actually filled on, so it closes after exactly
`time_stop_bars` candles (earlier versions closed one candle early on limit fills). The Pine script,
the Python model and the goldib bot now agree on this.
