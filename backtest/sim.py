"""Faithful Python port of the Gold Secret Inside-Bar & Fakeout Wick strategy (Pine v6).

Mirrors the Pine script bar-by-bar, including TradingView broker-emulator behaviour:
  * script runs at bar close; market entries fill at that close (process_orders_on_close)
  * exit stop/limit orders are evaluated against the NEXT bars' OHLC with the standard
    intrabar path assumption (open -> nearer extreme -> farther extreme -> close)
  * the breakeven stop update fires at script time on the bar whose high/low touches TP1,
    i.e. BEFORE the emulator processes that bar's fills (same quirk as the Pine code)
"""
import math
import numpy as np
import pandas as pd
from zoneinfo import ZoneInfo

ST_PENDING, ST_BROKEN, ST_VERIFIED, ST_TRADED, ST_CONSUMED, ST_INVALID, ST_EXPIRED = range(7)
SUP, RES = 1, -1
NY = ZoneInfo("America/New_York")

DEFAULTS = dict(
    tick=0.001,
    ib_tol_ticks=0, ib_color="any",            # any | colored
    swing_lookback=20, ctx_bars=3, sweep_wick_pct=0.35, min_sweep_atr=0.0, ib_tol_atr=0.0,
    level_mode="sweep",                        # sweep | swing_or_sweep
    key_level="off",                           # off | pd | pd_today | pd_today_pw
    key_level_tol_atr=0.5,
    atr_len=14, min_zone_atr=0.15, max_zone_atr=1.0,
    max_break_bars=5, min_impulse_mult=1.0, max_impulse_bars=10,
    confirm_mode="wick",                       # none | color | wick | both
    rej_wick_pct=0.40, confirm_close_outside=True, allow_deep_sweep=True,
    min_retest_depth=0.0,                      # fraction of zone height the retest must penetrate
    confirm_window=3, max_zone_age=200,
    sl_buffer_ticks=10, sl_buffer_atr=None,    # if sl_buffer_atr is set it overrides ticks
    tp1_rr=2.0, tp1_qty=0.5, tp2_rr=3.0, move_to_be=True, be_lock_r=0.0, trail_atr=0.0,
    time_stop_bars=0, min_risk_atr=0.30, max_risk_atr=2.0,
    entry_mode="market",                       # market | limit50 | limit_edge
    entry_valid_bars=5, limit_frac=0.5,           # limit price = low + frac*range (long), clipped to close
    use_trend=False, trend_len=200,
    use_session=True, session=("03:00", "16:00"),
    max_trades_day=3, max_zones=50,
    spread=0.0,                                # points, charged on entry (round-trip cost)
    risk_dollars=100.0,
)


class Zone:
    __slots__ = ("kind", "state", "top", "bottom", "mother_high", "mother_low", "swing_bar", "ib_bar",
                 "break_bar", "verified_bar", "retest_bar", "retest_extreme", "created_bar")

    def __init__(self, kind, top, bottom, mh, ml, swing_bar, ib_bar):
        self.kind, self.state, self.top, self.bottom = kind, ST_PENDING, top, bottom
        self.mother_high, self.mother_low = mh, ml
        self.swing_bar, self.ib_bar = swing_bar, ib_bar
        self.break_bar = self.verified_bar = self.retest_bar = -1
        self.retest_extreme = None

    @property
    def height(self):
        return self.top - self.bottom

    def alive(self):
        return self.state in (ST_PENDING, ST_BROKEN, ST_VERIFIED)


def rma(x, n):
    out = np.full(len(x), np.nan)
    if len(x) < n:
        return out
    a = 1.0 / n
    out[n - 1] = np.nanmean(x[:n])
    for i in range(n, len(x)):
        out[i] = a * x[i] + (1 - a) * out[i - 1]
    return out


def ema(x, n):
    out = np.full(len(x), np.nan)
    a = 2.0 / (n + 1)
    out[0] = x[0]
    for i in range(1, len(x)):
        out[i] = a * x[i] + (1 - a) * out[i - 1]
    return out


def rolling_max_prev(x, n):
    """highest(x, n)[1]: max over bars t-1 .. t-n"""
    s = pd.Series(x).rolling(n).max().shift(1).to_numpy()
    return s


def rolling_min_prev(x, n):
    return pd.Series(x).rolling(n).min().shift(1).to_numpy()


def prepare(df, cfg):
    """Pre-compute all series the strategy needs (global-scope calculations in Pine)."""
    o, h, l, c = (df[k].to_numpy(float) for k in ("open", "high", "low", "close"))
    n = len(df)
    pc = np.roll(c, 1); pc[0] = c[0]
    tr = np.maximum(h - l, np.maximum(abs(h - pc), abs(l - pc)))
    atr = rma(tr, cfg["atr_len"])
    rng = h - l
    uw = h - np.maximum(o, c)
    lw = np.minimum(o, c) - l
    uwp = np.where(rng > 0, uw / np.where(rng > 0, rng, 1), 0.0)
    lwp = np.where(rng > 0, lw / np.where(rng > 0, rng, 1), 0.0)
    L, K = cfg["swing_lookback"], cfg["ctx_bars"]
    prior_hi = rolling_max_prev(h, L)
    prior_lo = rolling_min_prev(l, L)
    ctx_hi = rolling_max_prev(h, K)
    ctx_lo = rolling_min_prev(l, K)
    ms = cfg["min_sweep_atr"] * atr
    sweeps_hi = (h > prior_hi + ms) & (c < prior_hi) & (uwp >= cfg["sweep_wick_pct"])
    sweeps_lo = (l < prior_lo - ms) & (c > prior_lo) & (lwp >= cfg["sweep_wick_pct"])
    sweep_hi_ctx = pd.Series(sweeps_hi.astype(int)).rolling(K).sum().shift(1).to_numpy() > 0
    sweep_lo_ctx = pd.Series(sweeps_lo.astype(int)).rolling(K).sum().shift(1).to_numpy() > 0
    tol = cfg["ib_tol_ticks"] * cfg["tick"] + cfg["ib_tol_atr"] * np.nan_to_num(atr)
    ph = np.roll(h, 1); pl = np.roll(l, 1)
    inside = (h <= ph + tol) & (l >= pl - tol)
    inside[: L + K + 3] = False
    # time features (New York)
    idx = df.index.tz_convert(NY) if df.index.tz is not None else df.index.tz_localize("UTC").tz_convert(NY)
    minutes = idx.hour * 60 + idx.minute
    s0 = int(cfg["session"][0][:2]) * 60 + int(cfg["session"][0][3:])
    s1 = int(cfg["session"][1][:2]) * 60 + int(cfg["session"][1][3:])
    in_session = (minutes >= s0) & (minutes < s1) if s0 <= s1 else (minutes >= s0) | (minutes < s1)
    if not cfg["use_session"]:
        in_session = np.ones(n, bool)
    # trading day boundary: 17:00 New York (OANDA / TradingView forex-CFD day)
    day_id = ((idx + pd.Timedelta(hours=7)).normalize()).to_numpy()
    new_day = np.zeros(n, bool); new_day[1:] = day_id[1:] != day_id[:-1]; new_day[0] = True
    # running day high/low, previous day H/L
    day_hi = np.full(n, np.nan); day_lo = np.full(n, np.nan)
    pdh = np.full(n, np.nan); pdl = np.full(n, np.nan)
    cur_h = cur_l = np.nan; prev_h = prev_l = np.nan
    for i in range(n):
        if new_day[i]:
            prev_h, prev_l = cur_h, cur_l
            cur_h, cur_l = h[i], l[i]
        else:
            cur_h = max(cur_h, h[i]); cur_l = min(cur_l, l[i])
        day_hi[i], day_lo[i], pdh[i], pdl[i] = cur_h, cur_l, prev_h, prev_l
    # previous week H/L (week starts Sunday 17:00 NY)
    wk = ((idx + pd.Timedelta(hours=7)).tz_localize(None).to_period("W-SAT")).to_numpy()
    new_wk = np.zeros(n, bool); new_wk[1:] = wk[1:] != wk[:-1]; new_wk[0] = True
    pwh = np.full(n, np.nan); pwl = np.full(n, np.nan)
    cur_h = cur_l = np.nan; prev_h = prev_l = np.nan
    for i in range(n):
        if new_wk[i]:
            prev_h, prev_l = cur_h, cur_l
            cur_h, cur_l = h[i], l[i]
        else:
            cur_h = max(cur_h, h[i]); cur_l = min(cur_l, l[i])
        pwh[i], pwl[i] = prev_h, prev_l
    trend = ema(c, cfg["trend_len"])
    return dict(o=o, h=h, l=l, c=c, atr=atr, uwp=uwp, lwp=lwp, prior_hi=prior_hi, prior_lo=prior_lo,
                ctx_hi=ctx_hi, ctx_lo=ctx_lo, sweep_hi_ctx=sweep_hi_ctx, sweep_lo_ctx=sweep_lo_ctx,
                inside=inside, in_session=in_session, new_day=new_day, day_hi=day_hi, day_lo=day_lo,
                pdh=pdh, pdl=pdl, pwh=pwh, pwl=pwl, trend=trend, n=n, ts=df.index)


def bar_path(o, h, l, c):
    """TradingView intrabar assumption: open -> nearer extreme -> farther extreme -> close."""
    if h - o < o - l:
        return (o, h, l, c)
    return (o, l, h, c)


def walk_exit(path, direction, tp, sl, start_price=None):
    """Return (exit_price, kind) or None. direction 1 long / -1 short.
    Price starts at path[0] (or start_price if the entry filled mid-path)."""
    pts = list(path)
    if start_price is not None:
        pts[0] = start_price
    p0 = pts[0]
    # gap / immediate checks at first point
    if direction == 1:
        if p0 <= sl:
            return p0, "sl"
        if p0 >= tp:
            return p0, "tp"
    else:
        if p0 >= sl:
            return p0, "sl"
        if p0 <= tp:
            return p0, "tp"
    for a, b in zip(pts[:-1], pts[1:]):
        if b > a:   # up segment
            if direction == 1 and a < tp <= b:
                return tp, "tp"
            if direction == -1 and a < sl <= b:
                return sl, "sl"
        elif b < a:  # down segment
            if direction == 1 and b <= sl < a:
                return sl, "sl"
            if direction == -1 and b <= tp < a:
                return tp, "tp"
    return None


def run(df, cfg=None, collect_trades=False):
    cfg = {**DEFAULTS, **(cfg or {})}
    S = prepare(df, cfg)
    o, h, l, c, atr = S["o"], S["h"], S["l"], S["c"], S["atr"]
    n = S["n"]
    tick = cfg["tick"]
    sweep_only = cfg["level_mode"] == "sweep"
    zones = []
    trades = []
    trades_today = 0
    diag = dict(cand=0, verified=0, confirmed=0, blk_intrade=0, blk_session=0, blk_cap=0,
                blk_trend=0, blk_risk=0, blk_keylevel=0, blk_fill=0, trades=0)
    # open trade state
    T = None  # dict with direction, entry, sl, tp1, tp2, legs, pending ...

    def clamp_h(raw, a):
        lo = max(cfg["min_zone_atr"] * a, tick)
        hi = max(cfg["max_zone_atr"] * a, lo)
        raw = raw if raw == raw else lo
        return min(hi, max(lo, raw))

    def confirm_ok(i, is_long):
        color_ok = c[i] > o[i] if is_long else c[i] < o[i]
        wick_ok = (S["lwp"][i] if is_long else S["uwp"][i]) >= cfg["rej_wick_pct"]
        m = cfg["confirm_mode"]
        return True if m == "none" else color_ok if m == "color" else wick_ok if m == "wick" else (color_ok and wick_ok)

    def near_key_level(i, kind, ztop, zbot, off):
        mode = cfg["key_level"]
        if mode == "off":
            return True
        tol = cfg["key_level_tol_atr"] * atr[i]
        lv = []
        if kind == RES:
            lv.append(S["pdh"][i])
            if mode in ("pd_today", "pd_today_pw") and i - off - 1 >= 0:
                lv.append(S["day_hi"][i - off - 1])
            if mode == "pd_today_pw":
                lv.append(S["pwh"][i])
        else:
            lv.append(S["pdl"][i])
            if mode in ("pd_today", "pd_today_pw") and i - off - 1 >= 0:
                lv.append(S["day_lo"][i - off - 1])
            if mode == "pd_today_pw":
                lv.append(S["pwl"][i])
        return any(v == v and (zbot - tol) <= v <= (ztop + tol) for v in lv)

    for i in range(n):
        a = atr[i]
        if S["new_day"][i]:
            trades_today = 0
        sl_buf = cfg["sl_buffer_atr"] * a if cfg["sl_buffer_atr"] is not None else cfg["sl_buffer_ticks"] * tick

        # ---------------- script step 1: trade state housekeeping (as of fills through i-1)
        if T is not None:
            if T["pending"]:
                if i - T["order_bar"] > T["valid_bars"]:
                    # cancel unfilled order, zone consumed without trade
                    diag["blk_fill"] += 1
                    T = None
            elif T["qty_open"] <= 1e-12:
                T = None

        # ---------------- script step 2: BE detection at script time (Pine quirk) + time stop
        close_now = False
        if T is not None and not T["pending"] and i > T["fill_bar"]:
            d = T["dir"]
            hit_tp1 = (h[i] >= T["tp1"]) if d == 1 else (l[i] <= T["tp1"])
            if hit_tp1 and not T["tp1_flag"]:
                T["tp1_flag"] = True
                if cfg["move_to_be"]:
                    T["sl2"] = T["entry"] + d * cfg["be_lock_r"] * T["risk"]
            if T["tp1_flag"] and cfg["trail_atr"] > 0:
                T["ext"] = max(T.get("ext", h[i]), h[i]) if d == 1 else min(T.get("ext", l[i]), l[i])
                trail = T["ext"] - d * cfg["trail_atr"] * a
                T["sl2"] = max(T["sl2"], trail) if d == 1 else min(T["sl2"], trail)
            if cfg["time_stop_bars"] > 0 and not T["tp1_flag"] and i - T["fill_bar"] >= cfg["time_stop_bars"]:
                close_now = True

        # ---------------- script step 3: zone state machine
        sig = None
        for z in list(zones):
            remove = False
            if z.state == ST_PENDING:
                if z.kind == SUP:
                    if c[i] < z.bottom:
                        remove = True
                    elif c[i] > z.mother_high:
                        z.state, z.break_bar = ST_BROKEN, i
                    elif i - z.ib_bar > cfg["max_break_bars"]:
                        remove = True
                else:
                    if c[i] > z.top:
                        remove = True
                    elif c[i] < z.mother_low:
                        z.state, z.break_bar = ST_BROKEN, i
                    elif i - z.ib_bar > cfg["max_break_bars"]:
                        remove = True
            elif z.state == ST_BROKEN:
                need = cfg["min_impulse_mult"] * z.height
                impulse = h[i] >= z.top + need if z.kind == SUP else l[i] <= z.bottom - need
                wrong = c[i] < z.bottom if z.kind == SUP else c[i] > z.top
                if wrong:
                    remove = True
                elif impulse:
                    z.state, z.verified_bar = ST_VERIFIED, i
                    diag["verified"] += 1
                elif i - z.break_bar > cfg["max_impulse_bars"]:
                    remove = True
            elif z.state == ST_VERIFIED:
                invalid = c[i] < z.bottom if z.kind == SUP else c[i] > z.top
                if invalid:
                    z.state = ST_INVALID
                elif i - z.verified_bar > cfg["max_zone_age"]:
                    z.state = ST_EXPIRED
                else:
                    depth = cfg["min_retest_depth"] * z.height
                    if z.kind == SUP:
                        touched = l[i] <= z.top - depth and (cfg["allow_deep_sweep"] or l[i] >= z.bottom - sl_buf)
                    else:
                        touched = h[i] >= z.bottom + depth and (cfg["allow_deep_sweep"] or h[i] <= z.top + sl_buf)
                    if touched and z.retest_bar < 0:
                        z.retest_bar = i
                    if touched:
                        ext = l[i] if z.kind == SUP else h[i]
                        z.retest_extreme = ext if z.retest_extreme is None else (min(z.retest_extreme, ext) if z.kind == SUP else max(z.retest_extreme, ext))
                    if z.retest_bar >= 0:
                        if z.kind == SUP:
                            close_ok = c[i] > z.top if cfg["confirm_close_outside"] else c[i] > z.bottom
                        else:
                            close_ok = c[i] < z.bottom if cfg["confirm_close_outside"] else c[i] < z.top
                        confirmed = touched and close_ok and confirm_ok(i, z.kind == SUP)
                        if confirmed and sig is None:
                            diag["confirmed"] += 1
                            sig = z
                            z.state = ST_TRADED
                        elif not confirmed and i - z.retest_bar >= cfg["confirm_window"]:
                            z.state = ST_CONSUMED
            if remove:
                zones.remove(z)

        # ---------------- script step 4: new candidate
        if S["inside"][i] and a == a and a > 0:
            res_ok = (cfg["ib_color"] == "any" or c[i] < o[i]) and (S["sweep_hi_ctx"][i] if sweep_only else (S["ctx_hi"][i] >= S["prior_hi"][i] or S["sweep_hi_ctx"][i]))
            sup_ok = (cfg["ib_color"] == "any" or c[i] > o[i]) and (S["sweep_lo_ctx"][i] if sweep_only else (S["ctx_lo"][i] <= S["prior_lo"][i] or S["sweep_lo_ctx"][i]))
            kind = 0
            if res_ok and sup_ok:
                kind = RES if c[i - 1] < o[i - 1] else SUP
            elif res_ok:
                kind = RES
            elif sup_ok:
                kind = SUP
            if kind != 0:
                K = cfg["ctx_bars"]
                if kind == RES:
                    top = max(S["ctx_hi"][i], h[i])
                    raw = top - min(max(o[i - 1], c[i - 1]), l[i])
                    bottom = top - clamp_h(raw, a)
                    off = next(k for k in range(1, K + 1) if h[i - k] == S["ctx_hi"][i])
                else:
                    bottom = min(S["ctx_lo"][i], l[i])
                    raw = max(min(o[i - 1], c[i - 1]), h[i]) - bottom
                    top = bottom + clamp_h(raw, a)
                    off = next(k for k in range(1, K + 1) if l[i - k] == S["ctx_lo"][i])
                dup = any(z.kind == kind and z.alive() and bottom <= z.top and top >= z.bottom for z in zones)
                if not dup and top > bottom:
                    if near_key_level(i, kind, top, bottom, off):
                        zones.append(Zone(kind, top, bottom, h[i - 1], l[i - 1], i - off, i))
                        diag["cand"] += 1
                        if len(zones) > cfg["max_zones"]:
                            zones.pop(0)
                    else:
                        diag["blk_keylevel"] += 1

        # ---------------- script step 5: entry
        if sig is not None:
            is_long = sig.kind == SUP
            d = 1 if is_long else -1
            flat = T is None
            trend_ok = (not cfg["use_trend"]) or (c[i] > S["trend"][i] if is_long else c[i] < S["trend"][i])
            cap_ok = cfg["max_trades_day"] == 0 or trades_today < cfg["max_trades_day"]
            allowed = flat and S["in_session"][i] and trend_ok and cap_ok
            if not allowed:
                if not flat: diag["blk_intrade"] += 1
                elif not S["in_session"][i]: diag["blk_session"] += 1
                elif not trend_ok: diag["blk_trend"] += 1
                else: diag["blk_cap"] += 1
                sig.state = ST_CONSUMED
            else:
                ext = sig.retest_extreme
                sl = (min(sig.bottom, ext if ext is not None else sig.bottom) - sl_buf) if is_long else (max(sig.top, ext if ext is not None else sig.top) + sl_buf)
                if cfg["entry_mode"] == "market":
                    entry = c[i]
                elif cfg["entry_mode"] == "limit50":
                    f = cfg["limit_frac"]
                    lim = l[i] + f * (h[i] - l[i]) if is_long else h[i] - f * (h[i] - l[i])
                    entry = min(lim, c[i]) if is_long else max(lim, c[i])
                else:
                    entry = min(sig.top, c[i]) if is_long else max(sig.bottom, c[i])
                risk = abs(entry - sl)
                risk_ok = risk >= tick and risk >= cfg["min_risk_atr"] * a and risk <= cfg["max_risk_atr"] * a
                if not risk_ok:
                    diag["blk_risk"] += 1
                    sig.state = ST_CONSUMED
                else:
                    tp1 = entry + d * risk * cfg["tp1_rr"]
                    tp2 = entry + d * risk * cfg["tp2_rr"]
                    qty = cfg["risk_dollars"] / risk
                    trades_today += 1
                    diag["trades"] += 1
                    T = dict(dir=d, entry=entry, sl=sl, sl2=sl, tp1=tp1, tp2=tp2, risk=risk, qty=qty,
                             qty_open=qty, leg1=qty * cfg["tp1_qty"], leg2=qty * (1 - cfg["tp1_qty"]),
                             tp1_flag=False, pnl=0.0, order_bar=i, fill_bar=i,
                             pending=cfg["entry_mode"] != "market",
                             valid_bars=cfg["entry_valid_bars"], signal_bar=i, exits=[])
                    if cfg["entry_mode"] == "market":
                        T["pnl"] -= cfg["spread"] * qty

        # ---------------- emulator: process bar i fills
        if T is not None:
            path = bar_path(o[i], h[i], l[i], c[i])
            d = T["dir"]
            start = None
            if T["pending"]:
                if i > T["order_bar"]:
                    # limit entry: does bar i touch the limit price?
                    lim = T["entry"]
                    fill = None
                    if (d == 1 and o[i] <= lim) or (d == -1 and o[i] >= lim):
                        fill = o[i]
                    else:
                        for aa, bb in zip(path[:-1], path[1:]):
                            if (d == 1 and bb <= lim < aa) or (d == -1 and aa < lim <= bb):
                                fill = lim; break
                    if fill is not None:
                        T["pending"] = False
                        T["fill_bar"] = i
                        T["entry"] = fill
                        T["pnl"] -= cfg["spread"] * T["qty"]
                        start = fill  # exits may trigger later in the same bar
                        # recompute TP/SL from actual fill keeps R semantics
                        T["tp1"] = fill + d * T["risk"] * cfg["tp1_rr"] if False else T["tp1"]
            if T is not None and not T["pending"] and (i > T["fill_bar"] or start is not None):
                # leg 1
                if T["leg1"] > 0:
                    r = walk_exit(path, d, T["tp1"], T["sl"], start)
                    if r is not None:
                        px, kind = r
                        T["pnl"] += d * (px - T["entry"]) * T["leg1"]
                        T["exits"].append((i, kind, "tp1" if kind == "tp" else "sl", px))
                        T["qty_open"] -= T["leg1"]; T["leg1"] = 0.0
                if T["leg2"] > 0:
                    r = walk_exit(path, d, T["tp2"], T["sl2"], start)
                    if r is not None:
                        px, kind = r
                        T["pnl"] += d * (px - T["entry"]) * T["leg2"]
                        T["exits"].append((i, kind, "tp2" if kind == "tp" else ("be" if abs(px - T["entry"]) < 1e-9 else "sl"), px))
                        T["qty_open"] -= T["leg2"]; T["leg2"] = 0.0
                if close_now and T["qty_open"] > 1e-12:
                    for leg in ("leg1", "leg2"):
                        if T[leg] > 0:
                            T["pnl"] += d * (c[i] - T["entry"]) * T[leg]
                            T["exits"].append((i, "time", leg, c[i]))
                            T["qty_open"] -= T[leg]; T[leg] = 0.0
                if T["qty_open"] <= 1e-12:
                    rec = dict(dir=d, signal_bar=T["signal_bar"], fill_bar=T["fill_bar"], exit_bar=i,
                               entry=T["entry"], sl=T["sl"], tp1=T["tp1"], tp2=T["tp2"], risk=T["risk"],
                               pnl=T["pnl"], R=T["pnl"] / cfg["risk_dollars"], exits=T["exits"],
                               time=S["ts"][T["fill_bar"]])
                    trades.append(rec)
                    T = None
    return summarize(trades, diag, cfg, collect_trades)


def summarize(trades, diag, cfg, collect):
    R = np.array([t["R"] for t in trades]) if trades else np.array([])
    gp = R[R > 0].sum() if len(R) else 0.0
    gl = -R[R < 0].sum() if len(R) else 0.0
    eq = np.cumsum(R) if len(R) else np.array([0.0])
    dd = (np.maximum.accumulate(eq) - eq).max() if len(R) else 0.0
    tp1_hits = sum(any(e[2] == "tp1" for e in t["exits"]) for t in trades)
    tp2_hits = sum(any(e[2] == "tp2" for e in t["exits"]) for t in trades)
    out = dict(trades=len(trades), pf=(gp / gl if gl > 0 else float("inf") if gp > 0 else 0.0),
               netR=float(R.sum()) if len(R) else 0.0, expR=float(R.mean()) if len(R) else 0.0,
               win=float((R > 0).mean()) if len(R) else 0.0,
               tp1_rate=tp1_hits / len(trades) if trades else 0.0,
               tp2_rate=tp2_hits / len(trades) if trades else 0.0,
               maxddR=float(dd), longs=sum(t["dir"] == 1 for t in trades), **{"d_" + k: v for k, v in diag.items()})
    if collect:
        out["trade_list"] = trades
    return out


def load(path, start=None, end=None):
    df = pd.read_csv(path, parse_dates=["time"], index_col="time")
    if df.index.tz is None:
        df.index = df.index.tz_localize("UTC")
    if start:
        df = df[df.index >= pd.Timestamp(start, tz="UTC")]
    if end:
        df = df[df.index < pd.Timestamp(end, tz="UTC")]
    return df


if __name__ == "__main__":
    import sys, json
    df = load(sys.argv[1], *(sys.argv[2:4] if len(sys.argv) > 3 else (None, None)))
    res = run(df, {}, collect_trades=True)
    tl = res.pop("trade_list")
    print(json.dumps(res, indent=1, default=str))
    for t in tl[:40]:
        print(t["time"], "L" if t["dir"] == 1 else "S", f"entry {t['entry']:.2f} sl {t['sl']:.2f} risk {t['risk']:.2f} R {t['R']:+.2f}", [(e[2], round(e[3], 2)) for e in t["exits"]])
