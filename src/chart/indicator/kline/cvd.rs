// src/chart/indicator/kline/cvd.rs
//
// Cumulative Volume Delta indicator.
// CVD = running Σ(buy_qty - sell_qty) per session.
//
// Uses the same indicator_row + LinePlot infrastructure as VolumeIndicator,
// with a BTreeMap<u64, f32> keyed by timestamp (TimeBased) or index (TickBased).

use crate::chart::{
    Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::KlineIndicatorImpl,
        plot::line::LinePlot,
    },
};

use data::chart::{PlotData, kline::KlineDataPoint};
use exchange::{Kline, Trade};

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

pub struct CvdIndicator {
    cache: Caches,
    /// Keyed by timestamp (TimeBased) or bar index (TickBased).
    /// Value is the running session CVD at bar close.
    data: BTreeMap<u64, f32>,
    /// Running session total, kept in sync with data.
    session_cvd: f32,
}

impl CvdIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            data: BTreeMap::new(),
            session_cvd: 0.0,
        }
    }

    fn rebuild(&mut self, source: &PlotData<KlineDataPoint>) {
        self.data.clear();
        self.session_cvd = 0.0;

        match source {
            PlotData::TimeBased(ts) => {
                for (timestamp, dp) in &ts.datapoints {
                    let bar_delta: f32 = dp.footprint.trades.values()
                        .map(|g| f32::from(g.buy_qty) - f32::from(g.sell_qty))
                        .sum();
                    self.session_cvd += bar_delta;
                    self.data.insert(*timestamp, self.session_cvd);
                }
            }
            PlotData::TickBased(ta) => {
                for (idx, dp) in ta.datapoints.iter().enumerate() {
                    let bar_delta: f32 = dp.footprint.trades.values()
                        .map(|g| f32::from(g.buy_qty) - f32::from(g.sell_qty))
                        .sum();
                    self.session_cvd += bar_delta;
                    self.data.insert(idx as u64, self.session_cvd);
                }
            }
        }

        self.cache.clear_all();
    }

    fn indicator_elem<'a>(
        &'a self,
        main_chart: &'a ViewState,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        let plot = LinePlot::new(|v: &f32| *v)
            .stroke_width(1.5)
            .show_points(false)
            .padding(0.05);

        indicator_row(main_chart, &self.cache, plot, &self.data, visible_range)
    }
}

impl KlineIndicatorImpl for CvdIndicator {
    fn clear_all_caches(&mut self) {
        self.cache.clear_all();
    }

    fn clear_crosshair_caches(&mut self) {
        self.cache.clear_crosshair();
    }

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        self.indicator_elem(chart, visible_range)
    }

    fn rebuild_from_source(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    /// Called when historical klines are inserted (TimeBased mode).
    ///
    /// By the time this is called, `source` already has trade data applied to
    /// the new buckets (insert_hist_klines runs insert_trades_existing_buckets
    /// before notifying indicators). A full rebuild is therefore correct here
    /// and ensures CVD is populated for all historical bars, not just the
    /// live window.
    fn on_insert_klines(&mut self, _klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        // Note: in TimeBased mode this hook is NOT called by the chart
        // (insert_trades only dispatches to indicators for TickBased).
        // The match arm below is therefore only ever reached in TickBased mode,
        // but we keep it exhaustive for correctness.
        match source {
            PlotData::TimeBased(ts) => {
                // Recompute only the last bar that could have been updated.
                let Some((&latest_ts, latest_dp)) = ts.datapoints.iter().next_back() else {
                    return;
                };

                // Anchor off the last *completed* bar's CVD value.
                let prev_cvd = self
                    .data
                    .range(..latest_ts)
                    .next_back()
                    .map(|(_, v)| *v)
                    .unwrap_or(0.0);

                let current_bar_delta: f32 = latest_dp
                    .footprint
                    .trades
                    .values()
                    .map(|g| f32::from(g.buy_qty) - f32::from(g.sell_qty))
                    .sum();

                self.session_cvd = prev_cvd + current_bar_delta;
                self.data.insert(latest_ts, self.session_cvd);
            }
            PlotData::TickBased(ta) => {
                // `old_dp_len` is the bar count *before* this batch of trades
                // was inserted. The last already-completed bar is at index
                // `old_dp_len - 1` and hasn't changed, so we start from
                // `old_dp_len` (the first new-or-updated bar).
                //
                // Edge case: when `old_dp_len == 0` the series was empty and
                // we process everything from index 0.
                let start_idx = old_dp_len;

                // Seed session_cvd from the last bar we are NOT reprocessing.
                self.session_cvd = if old_dp_len > 0 {
                    self.data
                        .get(&(old_dp_len as u64 - 1))
                        .copied()
                        .unwrap_or(0.0)
                } else {
                    0.0
                };

                for (idx, dp) in ta.datapoints.iter().enumerate().skip(start_idx) {
                    let bar_delta: f32 = dp.footprint.trades.values()
                        .map(|g| f32::from(g.buy_qty) - f32::from(g.sell_qty))
                        .sum();
                    self.session_cvd += bar_delta;
                    self.data.insert(idx as u64, self.session_cvd);
                }
            }
        }

        self.cache.clear_all();
    }

    fn on_ticksize_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    fn on_basis_change(&mut self, source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    fn on_open_interest(&mut self, _pairs: &[exchange::OpenInterest]) {}
}
