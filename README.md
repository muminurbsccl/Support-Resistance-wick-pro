# Gold Secret Inside-Bar & Fakeout Wick Strategy [1:2 / 1:3 RR]

Open-source TradingView **Pine Script® v6 strategy** for the Gold (XAUUSD) 5-minute
inside-bar / fakeout-wick pinpoint entry. No external libraries.

File: `gold_secret_inside_bar_fakeout_wick_strategy.pine`

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
| Max trades per day | 2 | Avoids stacking losses on choppy days. |
| Session | 03:00–16:00 New York | 08:00–16:00 gives a higher PF (≈2.4) with ~40% fewer trades. |

Result on 5m with a 0.25-point spread: 125 trades/year, profit factor 1.92, +0.40R per
trade, 9 of 13 months positive, max drawdown 7.3R. On 15m: profit factor 1.27.
Past performance is not a guarantee; re-validate on your broker's feed.

## Usage

1. Open TradingView, Pine Editor, paste the script, click **Add to chart**.
2. Recommended: XAUUSD, 5-minute chart. Tune the ATR clamps and buffers for other symbols.

Drawings are capped (`max_zones`, `max_trade_viz`) so `max_boxes_count = 500` is never exceeded.
