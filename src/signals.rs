//! Turns engine events into Telegram messages.
//!
//! A signal message carries everything needed to place the trade by hand:
//! side, entry (limit or market), stop loss, TP1 (partial + breakeven) and TP2,
//! how long a limit order stays valid, the time stop, and the $ risk/reward at
//! the configured lot size. Follow-ups report the fill, a missed fill, TP1,
//! the final exit and the result in R and $.

use crate::config::{Params, SignalConfig};
use crate::engine::{ClosedTrade, Event, Exit, Reason, Signal};
use chrono::DateTime;
use chrono_tz::Tz;
use std::collections::HashMap;

pub struct Notifier {
    cfg: SignalConfig,
    p: Params,
    tf_min: u32,
    tz: Tz,
    /// Signals that were announced, by id (follow-ups are only sent for these).
    open: HashMap<u64, Signal>,
    /// Actual fill price per trade (a gap can fill a limit at a better price).
    fills: HashMap<u64, f64>,
}

impl Notifier {
    pub fn new(cfg: &SignalConfig, p: &Params, tf_min: u32) -> Self {
        Self {
            tz: cfg.display_tz.parse().unwrap_or(chrono_tz::UTC),
            cfg: cfg.clone(),
            p: p.clone(),
            tf_min,
            open: HashMap::new(),
            fills: HashMap::new(),
        }
    }

    fn px(&self, v: f64) -> String {
        format!("{:.*}", self.cfg.price_decimals, v)
    }

    fn time(&self, ms: i64, fmt: &str) -> String {
        DateTime::from_timestamp_millis(ms)
            .unwrap_or_default()
            .with_timezone(&self.tz)
            .format(fmt)
            .to_string()
    }

    fn oz(&self) -> f64 {
        self.cfg.lots * self.cfg.oz_per_lot
    }

    fn usd(&self, r: f64, risk: f64) -> String {
        let v = r * risk * self.oz();
        if v >= 0.0 {
            format!("+${v:.0}")
        } else {
            format!("−${:.0}", -v)
        }
    }

    fn wrap(&self, body: String) -> String {
        if self.cfg.prefix.trim().is_empty() {
            body
        } else {
            format!("{}\n{body}", self.cfg.prefix.trim())
        }
    }

    /// Messages for one bar's events, in order, each with its trade id.
    pub fn messages(&mut self, events: &[Event]) -> Vec<(u64, String)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < events.len() {
            match &events[i] {
                Event::Signal(s) => {
                    out.push((s.id, self.wrap(self.signal_text(s))));
                    self.open.insert(s.id, s.clone());
                }
                Event::Cancelled { id } => {
                    if let Some(s) = self.open.remove(id)
                        && self.cfg.updates
                    {
                        out.push((
                            *id,
                            self.wrap(format!(
                                "⌛ Not filled – {} LIMIT {} {} cancelled\n(signal {} {})",
                                side(s.dir),
                                self.cfg.ticker,
                                self.px(s.entry),
                                self.time(s.time + self.tf_min as i64 * 60_000, "%H:%M"),
                                self.cfg.display_tz_label
                            )),
                        ));
                    }
                }
                Event::Filled { id, price } => {
                    self.fills.insert(*id, *price);
                    if let Some(s) = self.open.get(id)
                        && self.cfg.updates
                        && !s.market
                    {
                        out.push((
                            *id,
                            self.wrap(format!(
                                "✅ Filled – {} {} @ {}\nSL {} · TP1 {} · TP2 {}",
                                side(s.dir),
                                self.cfg.ticker,
                                self.px(*price),
                                self.px(s.sl),
                                self.px(s.tp1),
                                self.px(s.tp2)
                            )),
                        ));
                    }
                }
                Event::Exit { id, .. } => {
                    // Collect this trade's exits on this bar, plus its close if any.
                    let mut exits: Vec<Exit> = Vec::new();
                    let mut closed: Option<&ClosedTrade> = None;
                    let mut j = i;
                    while j < events.len() {
                        match &events[j] {
                            Event::Exit { id: k, exit } if k == id => exits.push(*exit),
                            Event::Closed(ct) if ct.id == *id => {
                                closed = Some(ct);
                                j += 1;
                                break;
                            }
                            _ => break,
                        }
                        j += 1;
                    }
                    i = j;
                    if let Some(s) = self.open.get(id).cloned() {
                        if self.cfg.updates {
                            out.push((*id, self.wrap(self.exit_text(&s, &exits, closed))));
                        }
                        if closed.is_some() {
                            self.open.remove(id);
                            self.fills.remove(id);
                        }
                    }
                    continue;
                }
                Event::Closed(ct) => {
                    self.open.remove(&ct.id);
                    self.fills.remove(&ct.id);
                }
            }
            i += 1;
        }
        out
    }

    pub fn signal_text(&self, s: &Signal) -> String {
        let p = &self.p;
        let book = (p.tp1_qty * 100.0).round();
        let head = if s.market {
            format!("{} {} NOW", dot(s.dir), side(s.dir))
        } else {
            format!("{} {} LIMIT", dot(s.dir), side(s.dir))
        };
        let zone = if s.dir == 1 { "Support" } else { "Resistance" };
        let r_tp1 = p.tp1_qty * p.tp1_rr;
        let r_full = r_tp1 + (1.0 - p.tp1_qty) * p.tp2_rr;
        let mut t = format!(
            "{head} · {} {}m\n\
             Entry  {}\n\
             SL     {}  ({} pts)\n\
             TP1    {}  1:{} · book {book:.0}%{}\n\
             TP2    {}  1:{}\n",
            self.cfg.ticker,
            self.tf_min,
            self.px(s.entry),
            self.px(s.sl),
            self.px(s.risk),
            self.px(s.tp1),
            fmt_rr(p.tp1_rr),
            if p.move_to_be { ", SL → entry" } else { "" },
            self.px(s.tp2),
            fmt_rr(p.tp2_rr),
        );
        if !s.market {
            t.push_str(&format!(
                "⏳ Valid until {} {} – cancel if not filled\n",
                self.time(s.expires, "%H:%M"),
                self.cfg.display_tz_label
            ));
        }
        if p.time_stop_bars > 0 {
            t.push_str(&format!(
                "⏱ Close at market if TP1 is not hit within {} min\n",
                p.time_stop_bars as u32 * self.tf_min
            ));
        }
        t.push_str(&format!(
            "📐 {zone} {}–{} · fakeout wick + inside bar, first retest rejected\n",
            self.px(s.zone_bottom),
            self.px(s.zone_top)
        ));
        t.push_str(&format!(
            "💵 {:.2} lot: risk ${:.0} · TP1 then BE {} · TP1 + TP2 {}\n",
            self.cfg.lots,
            s.risk * self.oz(),
            self.usd(r_tp1, s.risk),
            self.usd(r_full, s.risk)
        ));
        t.push_str(&format!(
            "🕒 {} {}",
            self.time(s.time + self.tf_min as i64 * 60_000, "%a %d %b %H:%M"),
            self.cfg.display_tz_label
        ));
        t
    }

    fn exit_text(&self, s: &Signal, exits: &[Exit], closed: Option<&ClosedTrade>) -> String {
        let entry = self.fills.get(&s.id).copied().unwrap_or(s.entry);
        let leg_frac = |leg: u8| {
            if leg == 1 {
                self.p.tp1_qty
            } else {
                1.0 - self.p.tp1_qty
            }
        };
        let r_of = |x: &Exit| leg_frac(x.leg) * s.dir as f64 * (x.price - entry) / s.risk;
        let Some(ct) = closed else {
            // Partial exit: only TP1 (or a TP1-part stop) can happen without closing.
            let x = exits.first().copied();
            return match x {
                Some(x) if x.reason == Reason::Tp => format!(
                    "🎯 TP1 hit – {} {} @ {}\n{:.0}% booked ({:+.2}R, {}){}\nRunner targets TP2 {}",
                    side(s.dir),
                    self.cfg.ticker,
                    self.px(x.price),
                    self.p.tp1_qty * 100.0,
                    r_of(&x),
                    self.usd(r_of(&x), s.risk),
                    if self.p.move_to_be {
                        format!(" · SL moved to entry {}", self.px(entry))
                    } else {
                        String::new()
                    },
                    self.px(s.tp2)
                ),
                Some(x) => format!(
                    "ℹ️ {} {}: part closed @ {} ({:+.2}R)",
                    side(s.dir),
                    self.cfg.ticker,
                    self.px(x.price),
                    r_of(&x)
                ),
                None => String::new(),
            };
        };
        // Closing message.
        let total_r: f64 = ct
            .exits
            .iter()
            .map(|x| leg_frac(x.leg) * s.dir as f64 * (x.price - entry) / s.risk)
            .sum();
        let last = exits.last().copied().or(ct.exits.last().copied());
        let headline = match last.map(|x| (x.leg, x.reason)) {
            Some((2, Reason::Tp)) => format!("🏆 TP2 hit @ {}", self.px(ct.tp2)),
            Some((_, Reason::Breakeven)) => {
                format!("➖ Runner stopped at entry {}", self.px(entry))
            }
            Some((_, Reason::TimeStop)) => format!(
                "⏱ Time stop – closed @ {} (TP1 not reached in {} min)",
                self.px(last.map(|x| x.price).unwrap_or(entry)),
                self.p.time_stop_bars as u32 * self.tf_min
            ),
            _ => format!(
                "🛑 Stop loss hit @ {}",
                self.px(last.map(|x| x.price).unwrap_or(s.sl))
            ),
        };
        format!(
            "{headline}\n{} {} trade closed: {:+.2}R ({} at {:.2} lot)",
            side(s.dir),
            self.cfg.ticker,
            total_r,
            self.usd(total_r, s.risk),
            self.cfg.lots
        )
    }
}

fn side(dir: i8) -> &'static str {
    if dir == 1 { "BUY" } else { "SELL" }
}

fn dot(dir: i8) -> &'static str {
    if dir == 1 { "🟢" } else { "🔴" }
}

fn fmt_rr(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ClosedTrade, Event, Exit, Reason, Signal};

    fn sig() -> Signal {
        Signal {
            id: 7,
            dir: -1,
            time: 1_793_000_000_000,
            entry: 4025.467,
            sl: 4030.27,
            tp1: 4018.26,
            tp2: 4006.25,
            risk: 4.803,
            market: false,
            zone_top: 4029.9,
            zone_bottom: 4026.1,
            expires: 1_793_001_200_000,
        }
    }

    #[test]
    fn signal_message_has_levels_and_money() {
        let n = Notifier::new(&SignalConfig::default(), &Params::default(), 5);
        let t = n.signal_text(&sig());
        assert!(t.starts_with("🔴 SELL LIMIT · XAUUSD 5m"));
        assert!(t.contains("Entry  4025.47"));
        assert!(t.contains("SL     4030.27"));
        assert!(t.contains("TP1    4018.26  1:1.5 · book 50%, SL → entry"));
        assert!(t.contains("TP2    4006.25  1:4"));
        assert!(t.contains("risk $48"));
        assert!(t.contains("Valid until"));
    }

    #[test]
    fn follow_ups_only_for_announced_trades() {
        let mut n = Notifier::new(&SignalConfig::default(), &Params::default(), 5);
        let x = Exit {
            time: 0,
            leg: 1,
            reason: Reason::Stop,
            price: 4030.27,
        };
        let closed = ClosedTrade {
            id: 7,
            dir: -1,
            signal_time: 0,
            fill_time: 0,
            exit_time: 0,
            entry: 4025.467,
            sl: 4030.27,
            tp1: 4018.26,
            tp2: 4006.25,
            risk: 4.803,
            r: -1.0,
            exits: vec![x, Exit { leg: 2, ..x }],
        };
        let evs = vec![
            Event::Exit { id: 7, exit: x },
            Event::Exit {
                id: 7,
                exit: Exit { leg: 2, ..x },
            },
            Event::Closed(closed),
        ];
        assert!(
            n.messages(&evs).is_empty(),
            "unknown trade must stay silent"
        );
        assert_eq!(n.messages(&[Event::Signal(sig())]).len(), 1);
        assert_eq!(
            n.messages(&[Event::Filled {
                id: 7,
                price: 4025.467
            }])
            .len(),
            1
        );
        let m = n.messages(&evs);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].0, 7);
        assert!(m[0].1.contains("Stop loss hit"), "{}", m[0].1);
        assert!(m[0].1.contains("-1.00R"), "{}", m[0].1);
    }
}
