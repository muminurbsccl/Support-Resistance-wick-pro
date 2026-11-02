//! One OHLC bar. `time` is the bar's open time in Unix milliseconds (UTC).

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bar {
    pub time: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
}
