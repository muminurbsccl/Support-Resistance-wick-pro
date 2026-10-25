"""Download Dukascopy XAUUSD 1-minute candles (bid) for a date range and write a 5m CSV.
Field order in the .bi5 records is: time offset, open, close, low, high, volume."""
import lzma, struct, sys, os, datetime as dt, time, concurrent.futures as cf
import urllib.request, ssl
import pandas as pd

BASE = "https://datafeed.dukascopy.com/datafeed/XAUUSD/{y}/{m:02d}/{d:02d}/BID_candles_min_1.bi5"
CA = "/root/.ccr/ca-bundle.crt"
ctx = ssl.create_default_context(cafile=CA) if os.path.exists(CA) else ssl.create_default_context()

def fetch(day):
    url = BASE.format(y=day.year, m=day.month - 1, d=day.day)   # month is 0-based on Dukascopy
    err = None
    for attempt in range(6):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0"})
            with urllib.request.urlopen(req, timeout=60, context=ctx) as r:
                raw = r.read()
            if not raw:
                return day, []
            data = lzma.decompress(raw)
            base = dt.datetime(day.year, day.month, day.day, tzinfo=dt.timezone.utc)
            rows = []
            for i in range(0, len(data), 24):
                t, o, c, l, h, v = struct.unpack(">iiiiif", data[i:i + 24])
                if v > 0:
                    rows.append((base + dt.timedelta(seconds=t), o / 1000, h / 1000, l / 1000, c / 1000, v))
            return day, rows
        except Exception as e:
            err = e
            if getattr(e, "code", None) == 404:
                return day, []
            time.sleep(2 * (attempt + 1))
    print("FAIL", day, err, file=sys.stderr)
    return day, []

def download(start, end, out_5m):
    days = [start + dt.timedelta(n) for n in range((end - start).days + 1) if (start + dt.timedelta(n)).weekday() != 5]
    rows = []
    with cf.ThreadPoolExecutor(3) as ex:
        for _, r in ex.map(fetch, days):
            rows.extend(r)
    m1 = pd.DataFrame(rows, columns=["time", "open", "high", "low", "close", "volume"]).drop_duplicates("time").set_index("time").sort_index()
    bad = (m1.high < m1[["open", "close"]].max(axis=1)) | (m1.low > m1[["open", "close"]].min(axis=1))
    m1 = m1[~bad]
    r5 = m1.resample("5min", label="left", closed="left").agg({"open": "first", "high": "max", "low": "min", "close": "last", "volume": "sum"}).dropna()
    r5.to_csv(out_5m)
    return r5

if __name__ == "__main__":
    s = dt.date.fromisoformat(sys.argv[1]); e = dt.date.fromisoformat(sys.argv[2])
    df = download(s, e, sys.argv[3])
    print("5m bars", len(df), df.index.min(), df.index.max())
