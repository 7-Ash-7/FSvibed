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

    fn on_insert_klines(&mut self, _klines: &[Kline], source: &PlotData<KlineDataPoint>) {
        self.rebuild(source);
    }

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        old_dp_len: usize,
        source: &PlotData<KlineDataPoint>,
    ) {
        match source {
            PlotData::TimeBased(ts) => {
                let Some((&latest_ts, latest_dp)) = ts.datapoints.iter().next_back() else {
                    return;
                };

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
                // `old_dp_len` is the bar count *before* this trade batch was
                // inserted.  Two cases:
                //
                // A) No new bar created (trades updated the current open bar):
                //    old_dp_len == ta.datapoints.len(), e.g. both 50.
                //    The open bar is at index 49 = old_dp_len - 1.
                //    We must reprocess it, seeding from index 48.
                //
                // B) One (or more) new bars created:
                //    old_dp_len < ta.datapoints.len(), e.g. 49 vs 50.
                //    First new bar is at index 49 = old_dp_len.
                //    Bar 48 was already correct, seed from 48, start at 49.
                //
                // In both cases: start_idx = old_dp_len.saturating_sub(1),
                // seeding session_cvd from the bar at start_idx - 1.
                let start_idx = old_dp_len.saturating_sub(1);

                self.session_cvd = if start_idx > 0 {
                    self.data
                        .get(&(start_idx as u64 - 1))
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
