// src/chart/indicator/kline/cvd.rs
//
// Cumulative Volume Delta indicator.
//
// Two modes (switchable via CvdConfig):
//
// Classic mode (ema_mode = false):
//   CVD = running Σ(buy_qty - sell_qty) per session.
//   Produces an unbounded value that accumulates across bars.
//
// EMA-ratio mode (ema_mode = true):
//   CVD = EMA(buy_qty) / EMA(sell_qty)  per bar
//   Oscillates around 1.0: values > 1 mean buy pressure dominates,
//   < 1 means sell pressure dominates.

use crate::chart::{
    Caches, Message, ViewState,
    indicator::{
        indicator_row,
        kline::KlineIndicatorImpl,
        plot::line::LinePlot,
    },
};

use data::chart::{PlotData, kline::{CvdConfig, KlineDataPoint, KlineTrades}};
use exchange::{Kline, Trade};

use std::collections::BTreeMap;
use std::ops::RangeInclusive;

pub struct CvdIndicator {
    cache: Caches,
    /// Keyed by timestamp (TimeBased) or bar index (TickBased).
    data: BTreeMap<u64, f32>,
    /// Running session cumulative delta — used only in classic mode.
    session_cvd: f32,
    /// Running EMA of buy qty — used only in EMA-ratio mode.
    ema_buy: f32,
    /// Running EMA of sell qty — used only in EMA-ratio mode.
    ema_sell: f32,
    /// Current configuration.
    config: CvdConfig,
}

impl CvdIndicator {
    pub fn new() -> Self {
        Self {
            cache: Caches::default(),
            data: BTreeMap::new(),
            session_cvd: 0.0,
            ema_buy: 0.0,
            ema_sell: 0.0,
            config: CvdConfig::default(),
        }
    }

    /// EMA smoothing factor α = 2 / (period + 1).
    #[inline]
    fn alpha(period: u32) -> f32 {
        2.0 / (period as f32 + 1.0)
    }

    /// Advance a running EMA by one bar value.
    #[inline]
    fn update_ema(prev: f32, new_value: f32, alpha: f32) -> f32 {
        alpha * new_value + (1.0 - alpha) * prev
    }

    fn rebuild(&mut self, source: &PlotData<KlineDataPoint>) {
        self.data.clear();
        self.session_cvd = 0.0;
        self.ema_buy = 0.0;
        self.ema_sell = 0.0;

        let alpha = Self::alpha(self.config.ema_period);

        match source {
            PlotData::TimeBased(ts) => {
                for (timestamp, dp) in &ts.datapoints {
                    let value = self.compute_bar(&dp.footprint, alpha);
                    self.data.insert(*timestamp, value);
                }
            }
            PlotData::TickBased(ta) => {
                for (idx, dp) in ta.datapoints.iter().enumerate() {
                    let value = self.compute_bar(&dp.footprint, alpha);
                    self.data.insert(idx as u64, value);
                }
            }
        }

        self.cache.clear_all();
    }

    /// Advance the running state by one bar's footprint and return the new
    /// indicator value. Takes `&KlineTrades` directly so it works for both
    /// `KlineDataPoint` (TimeBased) and `TickAccumulation` (TickBased).
    fn compute_bar(&mut self, footprint: &KlineTrades, alpha: f32) -> f32 {
        let (buy_qty, sell_qty): (f32, f32) = footprint
            .trades
            .values()
            .fold((0.0, 0.0), |(b, s), g| {
                (b + f32::from(g.buy_qty), s + f32::from(g.sell_qty))
            });

        if self.config.ema_mode {
            self.ema_buy = Self::update_ema(self.ema_buy, buy_qty, alpha);
            self.ema_sell = Self::update_ema(self.ema_sell, sell_qty, alpha);
            if self.ema_sell == 0.0 { 1.0 } else { self.ema_buy / self.ema_sell }
        } else {
            self.session_cvd += buy_qty - sell_qty;
            self.session_cvd
        }
    }

    fn indicator_elem<'a>(
        &'a self,
        main_chart: &'a ViewState,
        visible_range: RangeInclusive<u64>,
    ) -> iced::Element<'a, Message> {
        let last_value = self.data.values().next_back().copied();

        let plot = LinePlot::new(|v: &f32| *v)
            .stroke_width(1.5)
            .show_points(false)
            .padding(0.05)
            .line_color(iced::Color::WHITE);

        indicator_row(
            main_chart,
            &self.cache,
            plot,
            &self.data,
            visible_range,
            last_value,
        )
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

    fn set_cvd_config(&mut self, config: CvdConfig) {
        if self.config != config {
            self.config = config;
            // Data is now stale. The caller (KlineChart::set_config) must
            // follow up with rebuild_from_source so the data is recomputed.
            self.data.clear();
            self.cache.clear_all();
        }
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
                // EMA-ratio mode: EMA state is stateful across all bars, so a
                // full rebuild is required unless per-bar EMA snapshots are
                // cached (they are not).
                if self.config.ema_mode {
                    self.rebuild(source);
                    return;
                }

                // Classic mode: only the live (last) bar changes.
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
                // EMA-ratio mode: full rebuild required.
                if self.config.ema_mode {
                    self.rebuild(source);
                    return;
                }

                // Classic mode: reprocess from the open bar that existed before
                // this batch of trades was inserted (old_dp_len - 1).
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
                    let bar_delta: f32 = dp
                        .footprint
                        .trades
                        .values()
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






