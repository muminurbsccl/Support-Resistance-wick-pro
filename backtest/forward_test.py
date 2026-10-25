"""Forward test: fetch data up to today, run the frozen v11 config, report trades filled on/after the cutoff.
Usage: python3 -I backtest/forward_test.py --cutoff 2026-10-26 [--history-start 2026-09-01] [--lots 0.1]"""
import sys, os, json, argparse, datetime as dt
import numpy as np, pandas as pd
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sim, fetch_dukascopy

ap = argparse.ArgumentParser()
ap.add_argument("--cutoff", required=True)
ap.add_argument("--history-start", default="2026-09-01")
ap.add_argument("--end", default=None)
ap.add_argument("--lots", type=float, default=0.1)
ap.add_argument("--data", default=None, help="use an existing 5m CSV instead of downloading")
a = ap.parse_args()
here = os.path.dirname(os.path.abspath(__file__))
cfg = json.load(open(os.path.join(here, "config_v11.json")))
cfg["session"] = tuple(cfg["session"])
end = dt.date.fromisoformat(a.end) if a.end else dt.date.today()
if a.data:
    df = sim.load(a.data, a.history_start, None)
else:
    path = os.path.join(here, "reports", f"xauusd_5m_{a.history_start}_{end}.csv")
    fetch_dukascopy.download(dt.date.fromisoformat(a.history_start), end, path)
    df = sim.load(path)
res = sim.run(df, cfg, collect_trades=True)
tl = [t for t in res["trade_list"] if str(t["time"]) >= a.cutoff]
oz = a.lots * 100
lines = [f"# Forward test report — generated {dt.datetime.now(dt.timezone.utc):%Y-%m-%d %H:%M} UTC",
         f"Data: Dukascopy XAUUSD 5m, {df.index.min():%Y-%m-%d} → {df.index.max():%Y-%m-%d %H:%M} UTC. Config: backtest/config_v11.json (frozen). Spread 0.25 pts. Size {a.lots} lot = {oz:.0f} oz.",
         f"Forward window: trades filled on/after {a.cutoff} UTC.", ""]
if not tl:
    lines.append("No trades filled in the forward window.")
else:
    R = np.array([t["R"] for t in tl]); risk = np.array([t["risk"] for t in tl]); usd = R * risk * oz
    gp, gl = usd[usd > 0].sum(), -usd[usd < 0].sum(); eq = np.cumsum(usd); dd = (np.maximum.accumulate(eq) - eq).max()
    lines += [f"Trades: {len(tl)} | winners {int((R > 0).sum())} | net {R.sum():+.2f}R | net ${usd.sum():+.0f} | PF {'n/a' if gl == 0 else f'{gp / gl:.2f}'} | max DD ${dd:.0f}", "",
              "| Fill time (UTC) | Side | Entry | SL | TP1 | TP2 | Risk pts | Result R | $ | Exits |", "|---|---|---|---|---|---|---|---|---|---|"]
    for t, u in zip(tl, usd):
        ex = ", ".join(f"{e[2]}@{e[3]:.2f}" for e in t["exits"])
        lines.append(f"| {str(t['time'])[:16]} | {'LONG' if t['dir'] == 1 else 'SHORT'} | {t['entry']:.2f} | {t['sl']:.2f} | {t['tp1']:.2f} | {t['tp2']:.2f} | {t['risk']:.2f} | {t['R']:+.2f} | {u:+.0f} | {ex} |")
# context: the 4 weeks before the cutoff with the same code
prior = [t for t in res["trade_list"] if str(t["time"]) < a.cutoff and str(t["time"]) >= str(pd.Timestamp(a.cutoff) - pd.Timedelta(days=28))[:10]]
if prior:
    Rp = np.array([t["R"] for t in prior]); glp = -Rp[Rp < 0].sum()
    lines += ["", f"Context, same code on the 4 weeks before the cutoff: {len(prior)} trades, net {Rp.sum():+.1f}R, PF {'n/a' if glp == 0 else f'{Rp[Rp > 0].sum() / glp:.2f}'}."]
report = "\n".join(lines)
out = os.path.join(here, "reports", f"forward_{a.cutoff}_to_{end}.md")
open(out, "w").write(report + "\n")
print(report); print("\nsaved:", out)
