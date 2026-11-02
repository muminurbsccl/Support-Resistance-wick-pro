//! Backtest statistics and the Markdown report.

use crate::engine::{ClosedTrade, Diag, Reason};
use chrono::DateTime;
use std::collections::BTreeMap;
use std::fmt::Write;

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub trades: usize,
    pub wins: usize,
    pub losses: usize,
    pub net_r: f64,
    pub gross_win_r: f64,
    pub gross_loss_r: f64,
    pub max_dd_r: f64,
    pub max_losing_streak: usize,
    pub net_usd: f64,
    pub gross_win_usd: f64,
    pub gross_loss_usd: f64,
    pub max_dd_usd: f64,
    pub worst_usd: f64,
    pub avg_risk_usd: f64,
}

impl Stats {
    pub fn from(trades: &[&ClosedTrade], oz: f64) -> Self {
        let mut s = Stats {
            trades: trades.len(),
            ..Default::default()
        };
        let (mut eq_r, mut peak_r, mut eq_u, mut peak_u, mut streak) = (0.0, 0.0, 0.0, 0.0f64, 0);
        let mut risk_sum = 0.0;
        for t in trades {
            let u = t.r * t.risk * oz;
            risk_sum += t.risk * oz;
            s.net_r += t.r;
            s.net_usd += u;
            if t.r > 0.0 {
                s.wins += 1;
                s.gross_win_r += t.r;
                s.gross_win_usd += u;
                streak = 0;
            } else if t.r < 0.0 {
                s.losses += 1;
                s.gross_loss_r -= t.r;
                s.gross_loss_usd -= u;
                streak += 1;
                s.max_losing_streak = s.max_losing_streak.max(streak);
            } else {
                streak = 0;
            }
            s.worst_usd = s.worst_usd.min(u);
            eq_r += t.r;
            peak_r = f64::max(peak_r, eq_r);
            s.max_dd_r = s.max_dd_r.max(peak_r - eq_r);
            eq_u += u;
            peak_u = peak_u.max(eq_u);
            s.max_dd_usd = s.max_dd_usd.max(peak_u - eq_u);
        }
        if !trades.is_empty() {
            s.avg_risk_usd = risk_sum / trades.len() as f64;
        }
        s
    }

    pub fn pf_r(&self) -> Option<f64> {
        (self.gross_loss_r > 0.0).then(|| self.gross_win_r / self.gross_loss_r)
    }

    pub fn pf_usd(&self) -> Option<f64> {
        (self.gross_loss_usd > 0.0).then(|| self.gross_win_usd / self.gross_loss_usd)
    }

    pub fn win_rate(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.wins as f64 / self.trades as f64 * 100.0
        }
    }

    pub fn exp_r(&self) -> f64 {
        if self.trades == 0 {
            0.0
        } else {
            self.net_r / self.trades as f64
        }
    }
}

fn pf(v: Option<f64>, has_wins: bool) -> String {
    match v {
        Some(x) => format!("{x:.2}"),
        None if has_wins => "∞".into(),
        None => "–".into(),
    }
}

fn usd(v: f64) -> String {
    if v < 0.0 {
        format!("−${:.0}", -v)
    } else {
        format!("${v:.0}")
    }
}

fn t_utc(ms: i64, fmt: &str) -> String {
    DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format(fmt)
        .to_string()
}

pub struct ReportInput<'a> {
    pub title: String,
    pub source: String,
    pub bars: usize,
    pub first_bar: i64,
    pub last_bar: i64,
    pub trades: &'a [ClosedTrade],
    pub diag: &'a Diag,
    pub lots: f64,
    pub oz_per_lot: f64,
    pub spread: f64,
    pub settings: String,
}

pub fn markdown(inp: &ReportInput) -> String {
    let oz = inp.lots * inp.oz_per_lot;
    let all: Vec<&ClosedTrade> = inp.trades.iter().collect();
    let s = Stats::from(&all, oz);
    let longs: Vec<&ClosedTrade> = all.iter().copied().filter(|t| t.dir == 1).collect();
    let shorts: Vec<&ClosedTrade> = all.iter().copied().filter(|t| t.dir == -1).collect();
    let sl = Stats::from(&longs, oz);
    let ss = Stats::from(&shorts, oz);
    let has = |t: &ClosedTrade, f: &dyn Fn(u8, Reason) -> bool| {
        t.exits.iter().any(|x| f(x.leg, x.reason))
    };
    let tp1 = all
        .iter()
        .filter(|t| has(t, &|l, r| l == 1 && r == Reason::Tp))
        .count();
    let tp2 = all
        .iter()
        .filter(|t| has(t, &|l, r| l == 2 && r == Reason::Tp))
        .count();
    let tstop = all
        .iter()
        .filter(|t| has(t, &|_, r| r == Reason::TimeStop))
        .count();
    let pct = |n: usize| {
        if s.trades > 0 {
            n as f64 * 100.0 / s.trades as f64
        } else {
            0.0
        }
    };

    let mut o = String::new();
    let _ = writeln!(o, "# {}\n", inp.title);
    let _ = writeln!(
        o,
        "Data: {} · {} bars · {} → {} UTC · cost {} pts per trade · size {:.2} lot ({} oz)\n",
        inp.source,
        inp.bars,
        t_utc(inp.first_bar, "%Y-%m-%d"),
        t_utc(inp.last_bar, "%Y-%m-%d %H:%M"),
        inp.spread,
        inp.lots,
        oz
    );
    let _ = writeln!(o, "## Summary\n");
    let _ = writeln!(o, "| Metric | All | Long | Short |\n|---|---|---|---|");
    let row = |o: &mut String, name: &str, f: &dyn Fn(&Stats) -> String| {
        let _ = writeln!(o, "| {name} | {} | {} | {} |", f(&s), f(&sl), f(&ss));
    };
    row(&mut o, "Trades", &|x| x.trades.to_string());
    row(&mut o, "Win rate", &|x| format!("{:.0}%", x.win_rate()));
    row(&mut o, "Profit factor (R)", &|x| pf(x.pf_r(), x.wins > 0));
    row(&mut o, "Net R", &|x| format!("{:+.2}R", x.net_r));
    row(&mut o, "Expectancy", &|x| format!("{:+.2}R", x.exp_r()));
    row(&mut o, "Max drawdown (R)", &|x| {
        format!("{:.2}R", x.max_dd_r)
    });
    row(
        &mut o,
        &format!("Net profit at {:.2} lot", inp.lots),
        &|x| usd(x.net_usd),
    );
    row(&mut o, "Profit factor ($)", &|x| pf(x.pf_usd(), x.wins > 0));
    row(&mut o, "Max drawdown ($)", &|x| usd(x.max_dd_usd));
    row(&mut o, "Worst trade ($)", &|x| usd(x.worst_usd));
    row(&mut o, "Average risk per trade ($)", &|x| {
        usd(x.avg_risk_usd)
    });
    row(&mut o, "Longest losing streak", &|x| {
        x.max_losing_streak.to_string()
    });
    let _ = writeln!(
        o,
        "\nExits: TP1 reached in {:.0}% of trades, TP2 in {:.0}%, time stop in {:.0}%.\n",
        pct(tp1),
        pct(tp2),
        pct(tstop)
    );

    // Monthly
    let mut months: BTreeMap<String, Vec<&ClosedTrade>> = BTreeMap::new();
    for t in &all {
        months
            .entry(t_utc(t.fill_time, "%Y-%m"))
            .or_default()
            .push(t);
    }
    if !months.is_empty() {
        let _ = writeln!(
            o,
            "## By month\n\n| Month | Trades | Wins | Net R | Net $ |\n|---|---|---|---|---|"
        );
        for (m, ts) in &months {
            let x = Stats::from(ts, oz);
            let _ = writeln!(
                o,
                "| {m} | {} | {} | {:+.2}R | {} |",
                x.trades,
                x.wins,
                x.net_r,
                usd(x.net_usd)
            );
        }
        let pos = months
            .values()
            .filter(|ts| Stats::from(ts, oz).net_r > 0.0)
            .count();
        let _ = writeln!(o, "\n{pos} of {} months positive.\n", months.len());
    }

    let d = inp.diag;
    let _ = writeln!(
        o,
        "## Signal funnel\n\nCandidate zones {} → verified {} → confirmed retests {} → orders {} (blocked: in trade {}, session {}, daily cap {}, risk size {}; limit not filled {}).\n",
        d.candidates,
        d.verified,
        d.confirmed,
        d.orders,
        d.blk_in_trade,
        d.blk_session,
        d.blk_daily_cap,
        d.blk_risk,
        d.unfilled
    );

    let _ = writeln!(
        o,
        "## Trades (UTC)\n\n| # | Fill time | Side | Entry | SL | TP1 | TP2 | Risk | Exits | R | $ |\n|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for (n, t) in all.iter().enumerate() {
        let exits: Vec<String> = t
            .exits
            .iter()
            .map(|x| {
                let r = match x.reason {
                    Reason::Tp => format!("TP{}", x.leg),
                    Reason::Stop => "SL".into(),
                    Reason::Breakeven => "BE".into(),
                    Reason::TimeStop => "time".into(),
                };
                format!("{r} {:.2}", x.price)
            })
            .collect();
        let _ = writeln!(
            o,
            "| {} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {:+.2} | {} |",
            n + 1,
            t_utc(t.fill_time, "%Y-%m-%d %H:%M"),
            if t.dir == 1 { "BUY" } else { "SELL" },
            t.entry,
            t.sl,
            t.tp1,
            t.tp2,
            t.risk,
            exits.join(", "),
            t.r,
            usd(t.r * t.risk * oz)
        );
    }
    let _ = writeln!(o, "\n## Settings\n\n```toml\n{}```", inp.settings);
    o
}

/// One line per trade for spreadsheets.
pub fn trades_csv(trades: &[ClosedTrade], oz: f64) -> String {
    let mut o = String::from(
        "signal_time_utc,fill_time_utc,exit_time_utc,side,entry,sl,tp1,tp2,risk,exits,r,usd\n",
    );
    for t in trades {
        let exits: Vec<String> = t
            .exits
            .iter()
            .map(|x| {
                format!(
                    "leg{}:{:?}@{:.3}@{}",
                    x.leg,
                    x.reason,
                    x.price,
                    t_utc(x.time, "%H:%M")
                )
            })
            .collect();
        let _ = writeln!(
            o,
            "{},{},{},{},{:.3},{:.3},{:.3},{:.3},{:.3},{},{:.4},{:.2}",
            t_utc(t.signal_time, "%Y-%m-%d %H:%M"),
            t_utc(t.fill_time, "%Y-%m-%d %H:%M"),
            t_utc(t.exit_time, "%Y-%m-%d %H:%M"),
            if t.dir == 1 { "BUY" } else { "SELL" },
            t.entry,
            t.sl,
            t.tp1,
            t.tp2,
            t.risk,
            exits.join(" "),
            t.r,
            t.r * t.risk * oz
        );
    }
    o
}
