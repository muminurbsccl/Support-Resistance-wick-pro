# Gold IB Fakeout – backtest

Data: TradingView OANDA:XAUUSD · 5726 bars · 2026-10-04 → 2026-11-02 16:05 UTC · cost 0.25 pts per trade · size 0.10 lot (10 oz)

## Summary

| Metric | All | Long | Short |
|---|---|---|---|
| Trades | 7 | 2 | 5 |
| Win rate | 86% | 100% | 80% |
| Profit factor (R) | 6.27 | ∞ | 4.98 |
| Net R | +5.55R | +1.36R | +4.19R |
| Expectancy | +0.79R | +0.68R | +0.84R |
| Max drawdown (R) | 1.05R | 0.00R | 1.05R |
| Net profit at 0.10 lot | $313 | $129 | $184 |
| Profit factor ($) | 7.24 | ∞ | 4.66 |
| Max drawdown ($) | $50 | $0 | $50 |
| Worst trade ($) | −$50 | $0 | −$50 |
| Average risk per trade ($) | $55 | $90 | $40 |
| Longest losing streak | 1 | 0 | 1 |

Exits: TP1 reached in 71% of trades, TP2 in 14%, time stop in 14%.

## By month

| Month | Trades | Wins | Net R | Net $ |
|---|---|---|---|---|
| 2026-10 | 7 | 6 | +5.55R | $313 |

1 of 1 months positive.

## Signal funnel

Candidate zones 115 → verified 48 → confirmed retests 11 → orders 8 (blocked: in trade 0, session 2, daily cap 1, risk size 0; limit not filled 1).

## Trades (UTC)

| # | Fill time | Side | Entry | SL | TP1 | TP2 | Risk | Exits | R | $ |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | 2026-10-07 17:00 | SELL | 4109.94 | 4114.72 | 4102.78 | 4090.84 | 4.77 | SL 4114.72, SL 4114.72 | -1.05 | −$50 |
| 2 | 2026-10-09 13:35 | SELL | 4188.89 | 4192.53 | 4183.44 | 4174.36 | 3.63 | TP1 4183.44, BE 4188.89 | +0.68 | $25 |
| 3 | 2026-10-12 18:05 | SELL | 4171.25 | 4175.60 | 4164.73 | 4153.86 | 4.35 | time 4165.69, time 4165.69 | +1.22 | $53 |
| 4 | 2026-10-13 13:40 | SELL | 4185.62 | 4190.89 | 4177.72 | 4164.55 | 5.27 | TP1 4177.72, TP2 4164.55 | +2.70 | $142 |
| 5 | 2026-10-15 10:00 | SELL | 4158.44 | 4160.65 | 4155.12 | 4149.60 | 2.21 | TP1 4155.12, BE 4158.44 | +0.64 | $14 |
| 6 | 2026-10-26 19:35 | BUY | 4010.47 | 4008.51 | 4013.40 | 4018.29 | 1.95 | TP1 4013.40, BE 4010.47 | +0.62 | $12 |
| 7 | 2026-10-28 19:10 | BUY | 4043.21 | 4027.25 | 4067.16 | 4107.06 | 15.96 | TP1 4067.16, BE 4043.21 | +0.73 | $117 |

## Settings

```toml
tick = 0.001
ib_tol_ticks = 0.0
ib_tol_atr = 0.02
ib_color = "any"
swing_lookback = 30
ctx_bars = 3
sweep_wick_pct = 0.4
min_sweep_atr = 0.0
level_mode = "sweep"
atr_len = 14
min_zone_atr = 0.15
max_zone_atr = 1.0
max_break_bars = 5
min_impulse_mult = 0.5
max_impulse_bars = 5
confirm_mode = "wick"
rej_wick_pct = 0.45
confirm_close_outside = true
allow_deep_sweep = true
min_retest_depth = 0.0
confirm_window = 3
max_zone_age = 200
entry_mode = "limit"
limit_frac = 0.5
entry_valid_bars = 3
sl_buffer_ticks = 10.0
tp1_rr = 1.5
tp1_qty = 0.5
tp2_rr = 4.0
move_to_be = true
be_lock_r = 0.0
time_stop_bars = 12
min_risk_atr = 0.3
max_risk_atr = 2.0
use_session = true
session_start = "03:00"
session_end = "16:00"
max_trades_day = 1
max_zones = 50
```
