// src/chart/indicator/kline.rs

use crate::chart::{Message, ViewState};
use crate::connector::fetcher::FetchRange;

use data::chart::PlotData;
use data::chart::indicator::KlineIndicator;
use data::chart::kline::{CvdConfig, KlineDataPoint};
use exchange::{Kline, Timeframe, Trade};

pub mod cvd;
pub mod open_interest;
pub mod volume;

pub trait KlineIndicatorImpl {
    /// Clear all caches for a full redraw
    fn clear_all_caches(&mut self);

    /// Clear caches related to crosshair only
    /// e.g. tooltips and scale labels for a partial redraw
    fn clear_crosshair_caches(&mut self);

    fn element<'a>(
        &'a self,
        chart: &'a ViewState,
        visible_range: std::ops::RangeInclusive<u64>,
    ) -> iced::Element<'a, Message>;

    /// Push updated CVD configuration into the indicator.
    /// Default is a no-op so Volume and OpenInterest don't need to implement it.
    /// After calling this, invoke `rebuild_from_source` to recompute data.
    fn set_cvd_config(&mut self, _config: CvdConfig) {}

    /// If the indicator needs data fetching, return the required range
    fn fetch_range(&mut self, _ctx: &FetchCtx) -> Option<FetchRange> {
        None
    }

    /// Rebuild data using kline(OHLCV) source
    fn rebuild_from_source(&mut self, _source: &PlotData<KlineDataPoint>) {}

    /// Called when historical klines are inserted (TimeBased mode).
    /// `source` is the full data source *after* the klines and any buffered
    /// trades have been applied, so footprint data is already populated.
    fn on_insert_klines(&mut self, _klines: &[Kline], _source: &PlotData<KlineDataPoint>) {}

    fn on_insert_trades(
        &mut self,
        _trades: &[Trade],
        _old_dp_len: usize,
        _source: &PlotData<KlineDataPoint>,
    ) {
    }

    fn on_ticksize_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    /// Timeframe/tick interval has changed
    fn on_basis_change(&mut self, _source: &PlotData<KlineDataPoint>) {}

    fn on_open_interest(&mut self, _pairs: &[exchange::OpenInterest]) {}
}

pub struct FetchCtx<'a> {
    pub main_chart: &'a ViewState,
    pub timeframe: Timeframe,
    pub visible_earliest: u64,
    pub kline_latest: u64,
    pub prefetch_earliest: u64,
}

pub fn make_empty(which: KlineIndicator) -> Box<dyn KlineIndicatorImpl> {
    match which {
        KlineIndicator::Volume => Box::new(super::kline::volume::VolumeIndicator::new()),
        KlineIndicator::OpenInterest => {
            Box::new(super::kline::open_interest::OpenInterestIndicator::new())
        }
        KlineIndicator::Cvd => Box::new(super::kline::cvd::CvdIndicator::new()),
    }
}
