//! The Gold Secret Inside-Bar & Fakeout Wick strategy, one closed bar at a time.
//!
//! This is a line-by-line port of the frozen Python model (`backtest/sim.py`),
//! which was checked trade-by-trade against TradingView's OANDA strategy tester.
//! The same `Engine` drives the backtester and the live bot, so a live signal is
//! exactly what the backtest would have produced on the same bars.
//!
//! Per bar, in this order (mirrors the Pine script and its broker emulator):
//! 1. housekeeping: cancel a limit order that was not filled in time
//! 2. breakeven / time-stop bookkeeping at script time (Pine quirk kept on purpose)
//! 3. zone state machine: pending → broken → verified → first retest → confirmation
//! 4. new candidate zone: inside bar next to a fakeout-wick swing
//! 5. entry: one trade per zone, session / daily-cap / risk-size filters
//! 6. broker emulator: limit fill, TP1 / TP2 / stop / breakeven / time-stop exits
//!    using TradingView's intrabar path (open → nearer extreme → farther extreme → close)

use crate::bar::Bar;
use crate::config::{ConfirmMode, EntryMode, IbColor, LevelMode, Params, parse_hhmm};
use chrono::{DateTime, NaiveDate, Timelike};
use chrono_tz::America::New_York;

pub const SUP: i8 = 1;
pub const RES: i8 = -1;
const EPS: f64 = 1e-12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZoneState {
    Pending,
    Broken,
    Verified,
    Traded,
    Consumed,
    Invalid,
    Expired,
}

#[derive(Debug, Clone)]
pub struct Zone {
    pub kind: i8,
    pub state: ZoneState,
    pub top: f64,
    pub bottom: f64,
    mother_high: f64,
    mother_low: f64,
    ib_bar: usize,
    break_bar: usize,
    verified_bar: usize,
    retest_bar: Option<usize>,
    retest_extreme: Option<f64>,
}

impl Zone {
    fn height(&self) -> f64 {
        self.top - self.bottom
    }
    fn alive(&self) -> bool {
        matches!(
            self.state,
            ZoneState::Pending | ZoneState::Broken | ZoneState::Verified
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Tp,
    Stop,
    Breakeven,
    TimeStop,
}

#[derive(Debug, Clone, Copy)]
pub struct Exit {
    pub time: i64,
    /// 1 = the TP1 part, 2 = the runner.
    pub leg: u8,
    pub reason: Reason,
    pub price: f64,
}

/// What a signal looks like when it is announced.
#[derive(Debug, Clone)]
pub struct Signal {
    pub id: u64,
    pub dir: i8,
    /// Bar open time of the confirmation candle.
    pub time: i64,
    pub entry: f64,
    pub sl: f64,
    pub tp1: f64,
    pub tp2: f64,
    pub risk: f64,
    pub market: bool,
    pub zone_top: f64,
    pub zone_bottom: f64,
    /// Bar open time after which an unfilled limit order is cancelled.
    pub expires: i64,
}

#[derive(Debug, Clone)]
pub struct ClosedTrade {
    pub id: u64,
    pub dir: i8,
    pub signal_time: i64,
    pub fill_time: i64,
    pub exit_time: i64,
    pub entry: f64,
    pub sl: f64,
    pub tp1: f64,
    pub tp2: f64,
    pub risk: f64,
    /// Result in R, after the configured spread.
    pub r: f64,
    pub exits: Vec<Exit>,
}

#[derive(Debug, Clone)]
pub enum Event {
    Signal(Signal),
    /// Limit order not filled in time.
    Cancelled {
        id: u64,
    },
    Filled {
        id: u64,
        price: f64,
    },
    Exit {
        id: u64,
        exit: Exit,
    },
    Closed(ClosedTrade),
}

#[derive(Debug, Clone, Default)]
pub struct Diag {
    pub candidates: u64,
    pub verified: u64,
    pub confirmed: u64,
    pub blk_in_trade: u64,
    pub blk_session: u64,
    pub blk_daily_cap: u64,
    pub blk_risk: u64,
    pub unfilled: u64,
    pub orders: u64,
}

#[derive(Debug, Clone)]
struct Trade {
    id: u64,
    dir: i8,
    entry: f64,
    sl: f64,
    sl2: f64,
    tp1: f64,
    tp2: f64,
    risk: f64,
    qty: f64,
    qty_open: f64,
    leg1: f64,
    leg2: f64,
    tp1_flag: bool,
    pnl: f64,
    order_bar: usize,
    fill_bar: usize,
    pending: bool,
    signal_time: i64,
    exits: Vec<Exit>,
}

pub struct Engine {
    p: Params,
    spread: f64,
    tf_ms: i64,
    s0: u32,
    s1: u32,
    bars: Vec<Bar>,
    sweeps_hi: Vec<bool>,
    sweeps_lo: Vec<bool>,
    tr_seed: Vec<f64>,
    atr: f64,
    zones: Vec<Zone>,
    trade: Option<Trade>,
    trades_today: u32,
    day: Option<NaiveDate>,
    next_id: u64,
    pub diag: Diag,
}

/// Notional size used to express P&L in R (any value works: R = pnl / RISK_DOLLARS).
const RISK_DOLLARS: f64 = 100.0;

impl Engine {
    pub fn new(p: Params, spread: f64, timeframe_min: u32) -> Self {
        let s0 = parse_hhmm(&p.session_start).unwrap_or(180);
        let s1 = parse_hhmm(&p.session_end).unwrap_or(960);
        Self {
            p,
            spread,
            tf_ms: timeframe_min as i64 * 60_000,
            s0,
            s1,
            bars: Vec::new(),
            sweeps_hi: Vec::new(),
            sweeps_lo: Vec::new(),
            tr_seed: Vec::new(),
            atr: f64::NAN,
            zones: Vec::new(),
            trade: None,
            trades_today: 0,
            day: None,
            next_id: 1,
            diag: Diag::default(),
        }
    }

    pub fn last_bar(&self) -> Option<&Bar> {
        self.bars.last()
    }

    /// Zones currently waiting for a break or for their first retest.
    pub fn active_zones(&self) -> impl Iterator<Item = &Zone> {
        self.zones.iter().filter(|z| z.alive())
    }

    /// Whether an order is pending or a position is open.
    pub fn in_trade(&self) -> bool {
        self.trade.is_some()
    }

    fn max_prev(&self, i: usize, n: usize, high: bool) -> f64 {
        if i < n {
            return f64::NAN;
        }
        let it = self.bars[i - n..i].iter();
        if high {
            it.map(|b| b.high).fold(f64::NEG_INFINITY, f64::max)
        } else {
            it.map(|b| b.low).fold(f64::INFINITY, f64::min)
        }
    }

    /// Feeds one **closed** bar and returns what happened on it.
    pub fn on_bar(&mut self, b: Bar) -> Vec<Event> {
        let p = self.p.clone();
        let mut ev = Vec::new();
        let i = self.bars.len();
        let (o, h, l, c) = (b.open, b.high, b.low, b.close);

        // ── series (Pine global scope) ──
        let pc = if i == 0 { c } else { self.bars[i - 1].close };
        let tr = (h - l).max((h - pc).abs().max((l - pc).abs()));
        let n = p.atr_len;
        if i < n {
            self.tr_seed.push(tr);
            if i + 1 == n {
                self.atr = self.tr_seed.iter().sum::<f64>() / n as f64;
            }
        } else {
            let a = 1.0 / n as f64;
            self.atr = a * tr + (1.0 - a) * self.atr;
        }
        let a = self.atr;
        self.bars.push(b);

        let rng = h - l;
        let uwp = if rng > 0.0 { (h - o.max(c)) / rng } else { 0.0 };
        let lwp = if rng > 0.0 { (o.min(c) - l) / rng } else { 0.0 };
        let (lb, k) = (p.swing_lookback, p.ctx_bars);
        let prior_hi = self.max_prev(i, lb, true);
        let prior_lo = self.max_prev(i, lb, false);
        let ms = p.min_sweep_atr * a;
        self.sweeps_hi
            .push(h > prior_hi + ms && c < prior_hi && uwp >= p.sweep_wick_pct);
        self.sweeps_lo
            .push(l < prior_lo - ms && c > prior_lo && lwp >= p.sweep_wick_pct);
        let sweep_hi_ctx = i >= k && self.sweeps_hi[i - k..i].iter().any(|&x| x);
        let sweep_lo_ctx = i >= k && self.sweeps_lo[i - k..i].iter().any(|&x| x);
        let tol = p.ib_tol_ticks * p.tick + p.ib_tol_atr * if a.is_nan() { 0.0 } else { a };
        let inside = i >= lb + k + 3 && {
            let prev = self.bars[i - 1];
            h <= prev.high + tol && l >= prev.low - tol
        };
        let ny = DateTime::from_timestamp_millis(b.time)
            .unwrap_or_default()
            .with_timezone(&New_York);
        let minutes = ny.hour() * 60 + ny.minute();
        let in_session = !p.use_session
            || if self.s0 <= self.s1 {
                minutes >= self.s0 && minutes < self.s1
            } else {
                minutes >= self.s0 || minutes < self.s1
            };
        // Trading day starts 17:00 New York.
        let day = DateTime::from_timestamp_millis(b.time + 7 * 3_600_000)
            .unwrap_or_default()
            .with_timezone(&New_York)
            .date_naive();
        if self.day != Some(day) {
            self.day = Some(day);
            self.trades_today = 0;
        }
        let sl_buf = p.sl_buffer_ticks * p.tick;

        // ── 1. housekeeping ──
        if let Some(t) = &self.trade {
            if t.pending {
                if i - t.order_bar > p.entry_valid_bars {
                    self.diag.unfilled += 1;
                    ev.push(Event::Cancelled { id: t.id });
                    self.trade = None;
                }
            } else if t.qty_open <= EPS {
                self.trade = None;
            }
        }

        // ── 2. breakeven + time stop at script time ──
        let mut close_now = false;
        if let Some(t) = self.trade.as_mut()
            && !t.pending
            && i > t.fill_bar
        {
            let d = t.dir as f64;
            let hit_tp1 = if t.dir == 1 { h >= t.tp1 } else { l <= t.tp1 };
            if hit_tp1 && !t.tp1_flag {
                t.tp1_flag = true;
                if p.move_to_be {
                    t.sl2 = t.entry + d * p.be_lock_r * t.risk;
                }
            }
            if p.time_stop_bars > 0 && !t.tp1_flag && i - t.fill_bar >= p.time_stop_bars {
                close_now = true;
            }
        }

        // ── 3. zone state machine ──
        let mut sig: Option<usize> = None;
        let mut remove = vec![false; self.zones.len()];
        for (zi, z) in self.zones.iter_mut().enumerate() {
            let sup = z.kind == SUP;
            match z.state {
                ZoneState::Pending => {
                    if sup {
                        if c < z.bottom {
                            remove[zi] = true;
                        } else if c > z.mother_high {
                            z.state = ZoneState::Broken;
                            z.break_bar = i;
                        } else if i - z.ib_bar > p.max_break_bars {
                            remove[zi] = true;
                        }
                    } else if c > z.top {
                        remove[zi] = true;
                    } else if c < z.mother_low {
                        z.state = ZoneState::Broken;
                        z.break_bar = i;
                    } else if i - z.ib_bar > p.max_break_bars {
                        remove[zi] = true;
                    }
                }
                ZoneState::Broken => {
                    let need = p.min_impulse_mult * z.height();
                    let impulse = if sup {
                        h >= z.top + need
                    } else {
                        l <= z.bottom - need
                    };
                    let wrong = if sup { c < z.bottom } else { c > z.top };
                    if wrong {
                        remove[zi] = true;
                    } else if impulse {
                        z.state = ZoneState::Verified;
                        z.verified_bar = i;
                        self.diag.verified += 1;
                    } else if i - z.break_bar > p.max_impulse_bars {
                        remove[zi] = true;
                    }
                }
                ZoneState::Verified => {
                    let invalid = if sup { c < z.bottom } else { c > z.top };
                    if invalid {
                        z.state = ZoneState::Invalid;
                    } else if i - z.verified_bar > p.max_zone_age {
                        z.state = ZoneState::Expired;
                    } else {
                        let depth = p.min_retest_depth * z.height();
                        let touched = if sup {
                            l <= z.top - depth && (p.allow_deep_sweep || l >= z.bottom - sl_buf)
                        } else {
                            h >= z.bottom + depth && (p.allow_deep_sweep || h <= z.top + sl_buf)
                        };
                        if touched && z.retest_bar.is_none() {
                            z.retest_bar = Some(i);
                        }
                        if touched {
                            let ext = if sup { l } else { h };
                            z.retest_extreme = Some(match z.retest_extreme {
                                None => ext,
                                Some(x) if sup => x.min(ext),
                                Some(x) => x.max(ext),
                            });
                        }
                        if let Some(rb) = z.retest_bar {
                            let close_ok = if sup {
                                if p.confirm_close_outside {
                                    c > z.top
                                } else {
                                    c > z.bottom
                                }
                            } else if p.confirm_close_outside {
                                c < z.bottom
                            } else {
                                c < z.top
                            };
                            let color_ok = if sup { c > o } else { c < o };
                            let wick_ok = (if sup { lwp } else { uwp }) >= p.rej_wick_pct;
                            let confirm_ok = match p.confirm_mode {
                                ConfirmMode::None => true,
                                ConfirmMode::Color => color_ok,
                                ConfirmMode::Wick => wick_ok,
                                ConfirmMode::Both => color_ok && wick_ok,
                            };
                            let confirmed = touched && close_ok && confirm_ok;
                            if confirmed && sig.is_none() {
                                self.diag.confirmed += 1;
                                sig = Some(zi);
                                z.state = ZoneState::Traded;
                            } else if !confirmed && i - rb >= p.confirm_window {
                                z.state = ZoneState::Consumed;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Signal zone survives removal (it is Verified→Traded, never removed), so
        // capture it before compacting the list.
        let sig_zone = sig.map(|zi| self.zones[zi].clone());
        if remove.iter().any(|&r| r) {
            let mut it = remove.iter();
            self.zones.retain(|_| !*it.next().unwrap());
        }

        // ── 4. new candidate zone ──
        if inside && !a.is_nan() && a > 0.0 {
            let ctx_hi = self.max_prev(i, k, true);
            let ctx_lo = self.max_prev(i, k, false);
            let sweep_only = p.level_mode == LevelMode::Sweep;
            let any_color = p.ib_color == IbColor::Any;
            let res_ok = (any_color || c < o)
                && if sweep_only {
                    sweep_hi_ctx
                } else {
                    ctx_hi >= prior_hi || sweep_hi_ctx
                };
            let sup_ok = (any_color || c > o)
                && if sweep_only {
                    sweep_lo_ctx
                } else {
                    ctx_lo <= prior_lo || sweep_lo_ctx
                };
            let prev = self.bars[i - 1];
            let kind = match (res_ok, sup_ok) {
                (true, true) => {
                    if prev.close < prev.open {
                        RES
                    } else {
                        SUP
                    }
                }
                (true, false) => RES,
                (false, true) => SUP,
                _ => 0,
            };
            if kind != 0 {
                let clamp_h = |raw: f64| {
                    let lo = (p.min_zone_atr * a).max(p.tick);
                    let hi = (p.max_zone_atr * a).max(lo);
                    let raw = if raw.is_nan() { lo } else { raw };
                    hi.min(lo.max(raw))
                };
                let (top, bottom) = if kind == RES {
                    let top = ctx_hi.max(h);
                    let raw = top - prev.open.max(prev.close).min(l);
                    (top, top - clamp_h(raw))
                } else {
                    let bottom = ctx_lo.min(l);
                    let raw = prev.open.min(prev.close).max(h) - bottom;
                    (bottom + clamp_h(raw), bottom)
                };
                let dup = self
                    .zones
                    .iter()
                    .any(|z| z.kind == kind && z.alive() && bottom <= z.top && top >= z.bottom);
                if !dup && top > bottom {
                    self.zones.push(Zone {
                        kind,
                        state: ZoneState::Pending,
                        top,
                        bottom,
                        mother_high: prev.high,
                        mother_low: prev.low,
                        ib_bar: i,
                        break_bar: 0,
                        verified_bar: 0,
                        retest_bar: None,
                        retest_extreme: None,
                    });
                    self.diag.candidates += 1;
                    if self.zones.len() > p.max_zones {
                        self.zones.remove(0);
                    }
                }
            }
        }

        // ── 5. entry ──
        if let Some(z) = sig_zone {
            let is_long = z.kind == SUP;
            let d = if is_long { 1.0 } else { -1.0 };
            let flat = self.trade.is_none();
            let cap_ok = p.max_trades_day == 0 || self.trades_today < p.max_trades_day;
            let allowed = flat && in_session && cap_ok;
            // The zone is used up whether or not a trade is placed (one trade per zone):
            // its state is already terminal (Traded), which is all the logic needs.
            if !allowed {
                if !flat {
                    self.diag.blk_in_trade += 1;
                } else if !in_session {
                    self.diag.blk_session += 1;
                } else {
                    self.diag.blk_daily_cap += 1;
                }
            } else {
                let ext = z.retest_extreme;
                let sl = if is_long {
                    z.bottom.min(ext.unwrap_or(z.bottom)) - sl_buf
                } else {
                    z.top.max(ext.unwrap_or(z.top)) + sl_buf
                };
                let entry = match p.entry_mode {
                    EntryMode::Market => c,
                    EntryMode::Limit => {
                        let f = p.limit_frac;
                        if is_long {
                            (l + f * (h - l)).min(c)
                        } else {
                            (h - f * (h - l)).max(c)
                        }
                    }
                };
                let risk = (entry - sl).abs();
                let risk_ok =
                    risk >= p.tick && risk >= p.min_risk_atr * a && risk <= p.max_risk_atr * a;
                if !risk_ok {
                    self.diag.blk_risk += 1;
                } else {
                    let tp1 = entry + d * risk * p.tp1_rr;
                    let tp2 = entry + d * risk * p.tp2_rr;
                    let qty = RISK_DOLLARS / risk;
                    let market = p.entry_mode == EntryMode::Market;
                    self.trades_today += 1;
                    self.diag.orders += 1;
                    let id = self.next_id;
                    self.next_id += 1;
                    let mut t = Trade {
                        id,
                        dir: if is_long { 1 } else { -1 },
                        entry,
                        sl,
                        sl2: sl,
                        tp1,
                        tp2,
                        risk,
                        qty,
                        qty_open: qty,
                        leg1: qty * p.tp1_qty,
                        leg2: qty * (1.0 - p.tp1_qty),
                        tp1_flag: false,
                        pnl: 0.0,
                        order_bar: i,
                        fill_bar: i,
                        pending: !market,
                        signal_time: b.time,
                        exits: Vec::new(),
                    };
                    if market {
                        t.pnl -= self.spread * qty;
                    }
                    ev.push(Event::Signal(Signal {
                        id,
                        dir: t.dir,
                        time: b.time,
                        entry,
                        sl,
                        tp1,
                        tp2,
                        risk,
                        market,
                        zone_top: z.top,
                        zone_bottom: z.bottom,
                        expires: b.time + (p.entry_valid_bars as i64 + 1) * self.tf_ms,
                    }));
                    if market {
                        ev.push(Event::Filled { id, price: entry });
                    }
                    self.trade = Some(t);
                }
            }
        }

        // ── 6. broker emulator ──
        if let Some(t) = self.trade.as_mut() {
            let path = bar_path(o, h, l, c);
            let d = t.dir;
            let df = d as f64;
            let mut start: Option<f64> = None;
            if t.pending && i > t.order_bar {
                let lim = t.entry;
                let mut fill = None;
                if (d == 1 && o <= lim) || (d == -1 && o >= lim) {
                    fill = Some(o);
                } else {
                    for w in path.windows(2) {
                        let (aa, bb) = (w[0], w[1]);
                        if (d == 1 && bb <= lim && lim < aa) || (d == -1 && aa < lim && lim <= bb) {
                            fill = Some(lim);
                            break;
                        }
                    }
                }
                if let Some(f) = fill {
                    t.pending = false;
                    t.fill_bar = i;
                    t.entry = f;
                    t.pnl -= self.spread * t.qty;
                    start = Some(f);
                    ev.push(Event::Filled { id: t.id, price: f });
                }
            }
            if !t.pending && (i > t.fill_bar || start.is_some()) {
                if t.leg1 > 0.0
                    && let Some((px, is_tp)) = walk_exit(&path, d, t.tp1, t.sl, start)
                {
                    t.pnl += df * (px - t.entry) * t.leg1;
                    let x = Exit {
                        time: b.time,
                        leg: 1,
                        reason: if is_tp { Reason::Tp } else { Reason::Stop },
                        price: px,
                    };
                    t.exits.push(x);
                    ev.push(Event::Exit { id: t.id, exit: x });
                    t.qty_open -= t.leg1;
                    t.leg1 = 0.0;
                }
                if t.leg2 > 0.0
                    && let Some((px, is_tp)) = walk_exit(&path, d, t.tp2, t.sl2, start)
                {
                    t.pnl += df * (px - t.entry) * t.leg2;
                    let reason = if is_tp {
                        Reason::Tp
                    } else if (px - t.entry).abs() < 1e-9 {
                        Reason::Breakeven
                    } else {
                        Reason::Stop
                    };
                    let x = Exit {
                        time: b.time,
                        leg: 2,
                        reason,
                        price: px,
                    };
                    t.exits.push(x);
                    ev.push(Event::Exit { id: t.id, exit: x });
                    t.qty_open -= t.leg2;
                    t.leg2 = 0.0;
                }
                if close_now && t.qty_open > EPS {
                    for leg in [1u8, 2] {
                        let q = if leg == 1 { t.leg1 } else { t.leg2 };
                        if q > 0.0 {
                            t.pnl += df * (c - t.entry) * q;
                            let x = Exit {
                                time: b.time,
                                leg,
                                reason: Reason::TimeStop,
                                price: c,
                            };
                            t.exits.push(x);
                            ev.push(Event::Exit { id: t.id, exit: x });
                            t.qty_open -= q;
                            if leg == 1 {
                                t.leg1 = 0.0;
                            } else {
                                t.leg2 = 0.0;
                            }
                        }
                    }
                }
                if t.qty_open <= EPS {
                    let ct = ClosedTrade {
                        id: t.id,
                        dir: t.dir,
                        signal_time: t.signal_time,
                        fill_time: self.bars[t.fill_bar].time,
                        exit_time: b.time,
                        entry: t.entry,
                        sl: t.sl,
                        tp1: t.tp1,
                        tp2: t.tp2,
                        risk: t.risk,
                        r: t.pnl / RISK_DOLLARS,
                        exits: t.exits.clone(),
                    };
                    ev.push(Event::Closed(ct));
                    self.trade = None;
                }
            }
        }
        ev
    }
}

/// TradingView's intrabar assumption: open → nearer extreme → farther extreme → close.
fn bar_path(o: f64, h: f64, l: f64, c: f64) -> [f64; 4] {
    if h - o < o - l {
        [o, h, l, c]
    } else {
        [o, l, h, c]
    }
}

/// First exit hit along the path: `(price, is_take_profit)`.
fn walk_exit(
    path: &[f64; 4],
    dir: i8,
    tp: f64,
    sl: f64,
    start: Option<f64>,
) -> Option<(f64, bool)> {
    let mut pts = *path;
    if let Some(s) = start {
        pts[0] = s;
    }
    let p0 = pts[0];
    if dir == 1 {
        if p0 <= sl {
            return Some((p0, false));
        }
        if p0 >= tp {
            return Some((p0, true));
        }
    } else {
        if p0 >= sl {
            return Some((p0, false));
        }
        if p0 <= tp {
            return Some((p0, true));
        }
    }
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b > a {
            if dir == 1 && a < tp && tp <= b {
                return Some((tp, true));
            }
            if dir == -1 && a < sl && sl <= b {
                return Some((sl, false));
            }
        } else if b < a {
            if dir == 1 && b <= sl && sl < a {
                return Some((sl, false));
            }
            if dir == -1 && b <= tp && tp < a {
                return Some((tp, true));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays 11 weeks of Dukascopy XAUUSD 5m bars and requires exactly the trades
    /// the frozen Python model (`backtest/sim.py`, config v11, spread 0.25) produces.
    #[test]
    fn matches_python_model_trade_for_trade() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");
        let bars = crate::feed::csv::load(std::path::Path::new(&format!(
            "{dir}xauusd_5m_dukascopy_2026-08-17_2026-10-30.csv"
        )))
        .unwrap();
        let mut e = Engine::new(Params::default(), 0.25, 5);
        let mut got = Vec::new();
        for b in bars {
            for ev in e.on_bar(b) {
                if let Event::Closed(t) = ev {
                    got.push(t);
                }
            }
        }
        let want: Vec<Vec<f64>> = std::fs::read_to_string(format!("{dir}expected_trades_v11.csv"))
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| l.split(',').map(|v| v.parse().unwrap()).collect())
            .collect();
        assert_eq!(got.len(), want.len(), "trade count");
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.fill_time, w[0] as i64 * 1000);
            assert_eq!(g.dir as f64, w[1]);
            assert!(
                (g.entry - w[2]).abs() < 1e-6,
                "entry {} vs {}",
                g.entry,
                w[2]
            );
            assert!((g.sl - w[3]).abs() < 1e-6, "sl {} vs {}", g.sl, w[3]);
            assert!((g.r - w[4]).abs() < 1e-5, "R {} vs {}", g.r, w[4]);
        }
    }

    #[test]
    fn path_goes_to_nearer_extreme_first() {
        assert_eq!(bar_path(10.0, 11.0, 5.0, 8.0), [10.0, 11.0, 5.0, 8.0]);
        assert_eq!(bar_path(10.0, 15.0, 9.0, 12.0), [10.0, 9.0, 15.0, 12.0]);
    }

    #[test]
    fn exits_follow_the_path() {
        // Long: path dips to the stop before reaching the target.
        let p = bar_path(100.0, 106.0, 98.0, 104.0); // 100 → 98 → 106 → 104
        assert_eq!(walk_exit(&p, 1, 105.0, 99.0, None), Some((99.0, false)));
        // Short: target first.
        let p = bar_path(100.0, 101.0, 94.0, 95.0); // 100 → 101 → 94 → 95
        assert_eq!(walk_exit(&p, -1, 95.0, 102.0, None), Some((95.0, true)));
        // Gap through the stop fills at the open.
        assert_eq!(walk_exit(&p, 1, 110.0, 100.5, None), Some((100.0, false)));
        assert_eq!(walk_exit(&p, 1, 110.0, 90.0, None), None);
    }
}
