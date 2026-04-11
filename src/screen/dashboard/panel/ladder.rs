use super::Message;
use crate::style;
use data::panel::ladder::{ChaseTracker, Config, GroupedDepth, Side, TradeStore, ViewMode};
use exchange::Trade;
use exchange::unit::qty::Qty;
use exchange::unit::{Price, PriceStep};
use exchange::{TickerInfo, depth::Depth};

use iced::widget::canvas::{self, Path, Stroke, Text};
use iced::{Alignment, Event, Point, Rectangle, Renderer, Size, Theme, mouse};

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const TEXT_SIZE: f32 = 11.0;
const ROW_HEIGHT: f32 = 16.0;

// Total width ratios must sum to 1.0
/// Uses half of the width for each side of the order quantity columns
const ORDER_QTY_COLS_WIDTH: f32 = 0.60;
/// Uses half of the width for each side of the trade quantity columns
const TRADE_QTY_COLS_WIDTH: f32 = 0.20;

const COL_PADDING: f32 = 4.0;
/// Used for calculating layout with texts inside the price column
const MONO_CHAR_ADVANCE: f32 = 0.62;
/// Minimum padding on each side of the price text inside the price column
const PRICE_TEXT_SIDE_PAD_MIN: f32 = 12.0;

const CHASE_CIRCLE_RADIUS: f32 = 4.0;
/// Maximum interval between chase updates to consider them part of the same chase
const CHASE_MIN_INTERVAL: Duration = Duration::from_millis(200);

impl super::Panel for Ladder {
    fn scroll(&mut self, delta: f32) {
        self.scroll_px += delta;
        Ladder::invalidate(self, Some(Instant::now()));
    }

    fn reset_scroll(&mut self) {
        self.scroll_px = 0.0;
        Ladder::invalidate(self, Some(Instant::now()));
    }

    fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        Ladder::invalidate(self, now)
    }

    fn is_empty(&self) -> bool {
        if self.pending_tick_size.is_some() {
            return true;
        }
        self.grouped_asks().is_empty() && self.grouped_bids().is_empty() && self.trades.is_empty()
    }
}

pub struct Ladder {
    ticker_info: TickerInfo,
    pub config: Config,
    cache: canvas::Cache,
    last_tick: Instant,
    pub step: PriceStep,
    scroll_px: f32,
    last_exchange_ts_ms: Option<u64>,
    orderbook: [GroupedDepth; 2],
    trades: TradeStore,
    pending_tick_size: Option<PriceStep>,
    raw_price_spread: Option<Price>,
    backfill_done: bool,
}

impl Ladder {
    pub fn new(config: Option<Config>, ticker_info: TickerInfo, step: PriceStep) -> Self {
        Self {
            trades: TradeStore::new(),
            config: config.unwrap_or_default(),
            ticker_info,
            cache: canvas::Cache::default(),
            last_tick: Instant::now(),
            step,
            scroll_px: 0.0,
            last_exchange_ts_ms: None,
            orderbook: [GroupedDepth::new(), GroupedDepth::new()],
            raw_price_spread: None,
            pending_tick_size: None,
            backfill_done: false,
        }
    }

    pub fn insert_trades(&mut self, buffer: &[Trade]) {
        self.trades.insert_trades(buffer, self.step);
    }

    pub fn reset_backfill(&mut self) {
        self.backfill_done = false;
        self.trades = TradeStore::new();
    }

    pub fn insert_depth(&mut self, depth: &Depth, update_t: u64) {
        if let Some(next) = self.pending_tick_size.take() {
            self.step = next;
            self.trades.rebuild_grouped(self.step);
        }

        let raw_best_bid = depth.bids.last_key_value().map(|(p, _)| *p);
        let raw_best_ask = depth.asks.first_key_value().map(|(p, _)| *p);
        self.raw_price_spread = match (raw_best_bid, raw_best_ask) {
            (Some(bid), Some(ask)) => Some(ask - bid),
            _ => None,
        };

        if self.config.show_chase_tracker {
            let max_int = CHASE_MIN_INTERVAL;
            self.chase_tracker_mut(Side::Bid)
                .update(raw_best_bid, true, update_t, max_int);
            self.chase_tracker_mut(Side::Ask)
                .update(raw_best_ask, false, update_t, max_int);
        } else {
            self.chase_tracker_mut(Side::Bid).reset();
            self.chase_tracker_mut(Side::Ask).reset();
        }

        if self
            .trades
            .maybe_cleanup(update_t, self.config.trade_retention, self.step)
        {
            self.invalidate(Some(Instant::now()));
        }

        self.regroup_from_depth(depth);
        self.last_exchange_ts_ms = Some(update_t);
    }

    fn trade_qty_at(&self, price: Price) -> (Qty, Qty) {
        self.trades.trade_qty_at(price)
    }

    pub fn last_update(&self) -> Instant {
        self.last_tick
    }

    fn grouped_asks(&self) -> &BTreeMap<Price, Qty> {
        &self.orderbook[Side::Ask.idx()].orders
    }

    fn grouped_bids(&self) -> &BTreeMap<Price, Qty> {
        &self.orderbook[Side::Bid.idx()].orders
    }

    fn chase_tracker(&self, side: Side) -> &ChaseTracker {
        &self.orderbook[side.idx()].chase
    }

    fn chase_tracker_mut(&mut self, side: Side) -> &mut ChaseTracker {
        &mut self.orderbook[side.idx()].chase
    }

    fn best_price(&self, side: Side) -> Option<Price> {
        self.orderbook[side.idx()].best_price(side)
    }

    pub fn min_tick_size(&self) -> f32 {
        self.ticker_info.min_ticksize.into()
    }

    pub fn set_tick_size(&mut self, step: PriceStep) {
        self.pending_tick_size = Some(step);
        self.invalidate(Some(Instant::now()));
    }

    pub fn set_show_chase_tracker(&mut self, enabled: bool) {
        if self.config.show_chase_tracker != enabled {
            self.config.show_chase_tracker = enabled;
            if !enabled {
                self.chase_tracker_mut(Side::Bid).reset();
                self.chase_tracker_mut(Side::Ask).reset();
            }

            self.invalidate(Some(Instant::now()));
        }
    }

    fn regroup_from_depth(&mut self, depth: &Depth) {
        let step = self.step;

        self.orderbook[Side::Ask.idx()].regroup_from_raw(&depth.asks, Side::Ask, step);
        self.orderbook[Side::Bid.idx()].regroup_from_raw(&depth.bids, Side::Bid, step);
    }

    pub fn invalidate(&mut self, now: Option<Instant>) -> Option<super::Action> {
        self.cache.clear();
        if let Some(now) = now {
            self.last_tick = now;
        }

        // Emit a backfill request exactly once, after the ladder has received
        // live depth data (meaning the connection is up and ticker_info is valid).
        if self.config.backfill_enabled
            && !self.backfill_done
            && self.last_exchange_ts_ms.is_some()
        {
            self.backfill_done = true;
            let lookback_ms = self.config.backfill_lookback.to_millis();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;
            let from_ms = now_ms.saturating_sub(lookback_ms);
            return Some(super::Action::RequestBackfill {
                ticker_info: self.ticker_info,
                from_ms,
            });
        }

        None
    }

    fn format_price(&self, price: Price) -> String {
        let precision = self.ticker_info.min_ticksize;
        price.to_string(precision)
    }

    fn format_quantity(&self, qty: Qty) -> String {
        data::util::abbr_large_numbers(qty.to_f32_lossy())
    }
}

impl canvas::Program<Message> for Ladder {
    type State = ();

    fn update(
        &self,
        _state: &mut Self::State,
        event: &iced::Event,
        bounds: iced::Rectangle,
        cursor: iced_core::mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        let _cursor_position = cursor.position_in(bounds)?;

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(
                mouse::Button::Middle | mouse::Button::Left | mouse::Button::Right,
            )) => Some(canvas::Action::publish(Message::ResetScroll).and_capture()),
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let scroll_amount = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => -(*y) * ROW_HEIGHT,
                    mouse::ScrollDelta::Pixels { y, .. } => -*y,
                };

                Some(canvas::Action::publish(Message::Scrolled(scroll_amount)).and_capture())
            }
            _ => None,
        }
    }

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: iced_core::mouse::Cursor,
    ) -> Vec<iced::widget::canvas::Geometry<Renderer>> {
        let palette = theme.extended_palette();

        let text_color = palette.background.base.text;
        let bid_color = palette.success.base.color;
        let ask_color = palette.danger.base.color;
        let outline_color = Some(palette.warning.base.color.scale_alpha(0.5));

        let divider_color = style::split_ruler(theme).color;

        let orderbook_visual = self.cache.draw(renderer, bounds.size(), |frame| {
            if let Some(grid) = self.build_price_grid() {
                match self.config.view_mode {
                    ViewMode::Original => {
                        self.draw_original(
                            frame,
                            bounds,
                            &grid,
                            text_color,
                            bid_color,
                            ask_color,
                            divider_color,
                            outline_color,
                        );
                    }
                    ViewMode::StackedVP | ViewMode::DeltaVP => {
                        self.draw_vp(
                            frame,
                            bounds,
                            &grid,
                            text_color,
                            bid_color,
                            ask_color,
                            divider_color,
                            outline_color,
                            self.config.view_mode == ViewMode::StackedVP,
                        );
                    }
                }
            }
        });

        vec![orderbook_visual]
    }
}

#[derive(Default)]
struct Maxima {
    vis_max_order_qty: f32,
    vis_max_trade_qty: f32,
}

struct VisibleRow {
    row: DomRow,
    y: f32,
    buy_t: Qty,
    sell_t: Qty,
}

struct ColumnRanges {
    /// Shared order column - bid bars below spread, ask bars above spread
    order: (f32, f32),
    price: (f32, f32),
    /// Trade column - both buy and sell bars grow leftward from right_axis
    trade: (f32, f32),
    /// x position of the right-side volume axis (= trade.1)
    right_axis: f32,
}

/// Column layout for StackedVP mode
struct StackedVPColumnRanges {
    /// Order book column (left side, same as original)
    order: (f32, f32),
    price: (f32, f32),
    /// The stacked VP panel on the right (ask bar + bid bar anchored to right edge)
    vp: (f32, f32),
    /// Left edge of the VP panel (divider line position)
    vp_left: f32,
}

struct PriceLayout {
    price_px: f32,
    inside_pad_px: f32,
}

impl Ladder {
    fn price_sample_text(&self, grid: &PriceGrid) -> String {
        let a = self.format_price(grid.best_ask);
        let b = self.format_price(grid.best_bid);
        if a.len() >= b.len() { a } else { b }
    }

    fn mono_text_width_px(text_len: usize) -> f32 {
        (text_len as f32) * TEXT_SIZE * MONO_CHAR_ADVANCE
    }

    fn price_layout_for(&self, total_width: f32, grid: &PriceGrid) -> PriceLayout {
        let sample = self.price_sample_text(grid);
        let text_px = Self::mono_text_width_px(sample.len());

        let desired_total_gap = CHASE_CIRCLE_RADIUS * 2.0 + 4.0;
        let inside_pad_px = PRICE_TEXT_SIDE_PAD_MIN
            .max(desired_total_gap - COL_PADDING)
            .max(0.0);

        let price_px = (text_px + 2.0 * inside_pad_px).min(total_width.max(0.0));

        PriceLayout {
            price_px,
            inside_pad_px,
        }
    }

    fn column_ranges(&self, width: f32, price_px: f32) -> ColumnRanges {
        let right_axis = (width * 0.75).floor();
        let price_mid   = (width * 0.50).floor();

        let half_price = (price_px * 0.5).min(price_mid - COL_PADDING);
        let price_start = (price_mid - half_price).max(0.0);
        let price_end   = (price_mid + half_price).min(right_axis - COL_PADDING);
        let price_range = (price_start, price_end);

        let order_end   = (price_start - COL_PADDING).max(0.0);
        let order_range = (0.0, order_end);

        let trade_start = price_end + COL_PADDING;
        let trade_range = (trade_start, right_axis);

        ColumnRanges {
            order: order_range,
            price: price_range,
            trade: trade_range,
            right_axis,
        }
    }

    fn stacked_vp_column_ranges(&self, width: f32, price_px: f32) -> StackedVPColumnRanges {
        // The price column sits at the same position as in Original.
        // The VP panel fills everything to the right of the price column — no trade
        // columns, no extra axis. vp_left is simply price_end + COL_PADDING.

        let price_mid   = (width * 0.50).floor();
        let half_price  = (price_px * 0.5).min(price_mid - COL_PADDING);
        let price_start = (price_mid - half_price).max(0.0);
        let price_end   = price_mid + half_price;
        let order_end   = (price_start - COL_PADDING).max(0.0);
        let vp_left     = price_end + COL_PADDING;

        StackedVPColumnRanges {
            order: (0.0, order_end),
            price: (price_start, price_end),
            vp: (vp_left, width),
            vp_left,
        }
    }

    /// Original DOM/ladder rendering (unchanged behaviour)
    #[allow(clippy::too_many_arguments)]
    fn draw_original(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        bounds: Rectangle,
        grid: &PriceGrid,
        text_color: iced::Color,
        bid_color: iced::Color,
        ask_color: iced::Color,
        divider_color: iced::Color,
        outline_color: Option<iced::Color>,
    ) {
        let layout = self.price_layout_for(bounds.width, grid);
        let cols = self.column_ranges(bounds.width, layout.price_px);

        let (visible_rows, maxima) = self.visible_rows(bounds, grid);

        let mut spread_row: Option<(f32, f32)> = None;
        let mut best_bid_y: Option<f32> = None;
        let mut best_ask_y: Option<f32> = None;

        for visible_row in visible_rows.iter() {
            match visible_row.row {
                DomRow::Ask { price, .. }
                    if Some(price)
                        == self.grouped_asks().first_key_value().map(|(p, _)| *p) =>
                {
                    best_ask_y = Some(visible_row.y);
                }
                DomRow::Bid { price, .. }
                    if Some(price)
                        == self.grouped_bids().last_key_value().map(|(p, _)| *p) =>
                {
                    best_bid_y = Some(visible_row.y);
                }
                _ => {}
            }

            match visible_row.row {
                DomRow::Ask { price, qty } => {
                    self.draw_row(
                        frame,
                        visible_row.y,
                        price,
                        qty,
                        false,
                        bid_color,
                        ask_color,
                        text_color,
                        maxima.vis_max_order_qty,
                        visible_row.buy_t,
                        visible_row.sell_t,
                        maxima.vis_max_trade_qty,
                        bid_color,
                        ask_color,
                        &cols,
                        outline_color,
                        self.config.show_trade_text,
                    );
                }
                DomRow::Bid { price, qty } => {
                    self.draw_row(
                        frame,
                        visible_row.y,
                        price,
                        qty,
                        true,
                        bid_color,
                        ask_color,
                        text_color,
                        maxima.vis_max_order_qty,
                        visible_row.buy_t,
                        visible_row.sell_t,
                        maxima.vis_max_trade_qty,
                        bid_color,
                        ask_color,
                        &cols,
                        outline_color,
                        self.config.show_trade_text,
                    );
                }
                DomRow::Spread => {
                    if let Some(spread) = self.raw_price_spread {
                        let min_ticksize = self.ticker_info.min_ticksize;
                        spread_row = Some((visible_row.y, visible_row.y + ROW_HEIGHT));

                        let spread = spread.round_to_min_tick(min_ticksize);
                        let content = format!("Spread: {}", spread.to_string(min_ticksize));
                        frame.fill_text(Text {
                            content,
                            position: Point::new(
                                bounds.width / 2.0,
                                visible_row.y + ROW_HEIGHT / 2.0,
                            ),
                            color: text_color.scale_alpha(0.6),
                            size: (TEXT_SIZE - 1.0).into(),
                            font: style::AZERET_MONO,
                            align_x: Alignment::Center.into(),
                            align_y: Alignment::Center.into(),
                            ..Default::default()
                        });
                    }
                }
                DomRow::CenterDivider => {
                    let y_mid = visible_row.y + ROW_HEIGHT / 2.0 - 0.5;
                    frame.fill_rectangle(
                        Point::new(0.0, y_mid),
                        Size::new(bounds.width, 1.0),
                        divider_color,
                    );
                }
            }
        }

        if self.config.show_chase_tracker {
            let left_gap_mid_x = cols.order.1 + (layout.inside_pad_px + COL_PADDING) * 0.5;
            let right_gap_mid_x = cols.price.1 + (layout.inside_pad_px + COL_PADDING) * 0.5;

            self.draw_chase_trail(
                frame,
                grid,
                bounds,
                self.chase_tracker(Side::Bid),
                right_gap_mid_x,
                best_ask_y.map(|y| y + ROW_HEIGHT / 2.0),
                bid_color.scale_alpha(0.5),
                true,
            );
            self.draw_chase_trail(
                frame,
                grid,
                bounds,
                self.chase_tracker(Side::Ask),
                left_gap_mid_x,
                best_bid_y.map(|y| y + ROW_HEIGHT / 2.0),
                ask_color.scale_alpha(0.5),
                false,
            );
        }

        let mut draw_vsplit = |x: f32, gap: Option<(f32, f32)>| {
            let x = x.floor() + 0.5;
            match gap {
                Some((top, bottom)) => {
                    if top > 0.0 {
                        frame.fill_rectangle(
                            Point::new(x, 0.0),
                            Size::new(1.0, top.max(0.0)),
                            divider_color,
                        );
                    }
                    if bottom < bounds.height {
                        frame.fill_rectangle(
                            Point::new(x, bottom),
                            Size::new(1.0, (bounds.height - bottom).max(0.0)),
                            divider_color,
                        );
                    }
                }
                None => {
                    frame.fill_rectangle(
                        Point::new(x, 0.0),
                        Size::new(1.0, bounds.height),
                        divider_color,
                    );
                }
            }
        };
        draw_vsplit(cols.order.1, spread_row);
        draw_vsplit(cols.price.1, spread_row);
        {
            let x = cols.right_axis.floor() + 0.5;
            frame.fill_rectangle(
                Point::new(x, 0.0),
                Size::new(1.5, bounds.height),
                divider_color.scale_alpha(1.5),
            );
        }

        if let Some((top, bottom)) = spread_row {
            let y_top: f32 = top.floor() + 0.5;
            let y_bot = bottom.floor() + 0.5;

            frame.fill_rectangle(
                Point::new(0.0, y_top),
                Size::new(cols.order.1, 1.0),
                divider_color,
            );
            frame.fill_rectangle(
                Point::new(0.0, y_bot),
                Size::new(cols.order.1, 1.0),
                divider_color,
            );

            frame.fill_rectangle(
                Point::new(cols.price.1, y_top),
                Size::new(bounds.width - cols.price.1, 1.0),
                divider_color,
            );
            frame.fill_rectangle(
                Point::new(cols.price.1, y_bot),
                Size::new(bounds.width - cols.price.1, 1.0),
                divider_color,
            );
        }
    }

    /// VP panel rendering shared by StackedVP and DeltaVP modes.
    ///
    /// `stacked`: if true, draws ask bar as base + bid bar stacked on top (StackedVP).
    ///            if false, draws a single signed-delta bar (DeltaVP).
    ///
    /// In both modes the left side (order book + price column) is identical to Original,
    /// and the right VP panel replaces the trade columns.
    #[allow(clippy::too_many_arguments)]
    fn draw_vp(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        bounds: Rectangle,
        grid: &PriceGrid,
        text_color: iced::Color,
        bid_color: iced::Color,
        ask_color: iced::Color,
        divider_color: iced::Color,
        outline_color: Option<iced::Color>,
        stacked: bool,
    ) {
        let layout = self.price_layout_for(bounds.width, grid);
        let cols = self.stacked_vp_column_ranges(bounds.width, layout.price_px);

        let (visible_rows, maxima) = self.visible_rows(bounds, grid);

        // Scale VP bars against the maximum individual qty (bid or ask) across all visible rows,
        // so each bar fills the full VP width when it is the maximum.
        let max_single_vp_qty: f32 = visible_rows.iter().map(|r| {
            f32::from(r.buy_t).max(f32::from(r.sell_t))
        }).fold(0.0_f32, f32::max);

        let bar_h = ROW_HEIGHT - 1.0;
        let vp_width = cols.vp.1 - cols.vp.0;

        let mut spread_row: Option<(f32, f32)> = None;

        for visible_row in visible_rows.iter() {
            let y = visible_row.y;

            match visible_row.row {
                DomRow::Ask { price, qty } | DomRow::Bid { price, qty } => {
                    let is_bid = matches!(visible_row.row, DomRow::Bid { .. });
                    let side_color = if is_bid { bid_color } else { ask_color };
                    let order_alpha = if self.config.show_trade_text { 0.20 } else { 1.0 };

                    // Order book bar (left side, unchanged from Original)
                    Self::fill_bar(
                        frame,
                        cols.order,
                        y,
                        bar_h,
                        f32::from(qty),
                        maxima.vis_max_order_qty,
                        side_color,
                        true,
                        order_alpha,
                        outline_color,
                    );
                    if self.config.show_trade_text {
                        let qty_txt = self.format_quantity(qty);
                        Self::draw_cell_text(frame, &qty_txt, cols.order.0 + 4.0, y, text_color, Alignment::Start);
                    }

                    // Price label
                    let price_x_center = (cols.price.0 + cols.price.1) * 0.5;
                    let price_text = self.format_price(price);
                    Self::draw_cell_text(frame, &price_text, price_x_center, y, text_color, Alignment::Center);

                    // VP panel
                    let bid_qty_f32 = f32::from(visible_row.buy_t);
                    let ask_qty_f32 = f32::from(visible_row.sell_t);
                    let delta = bid_qty_f32 - ask_qty_f32;

                    if max_single_vp_qty > 0.0 {
                        let bar_alpha = if self.config.show_trade_text { 0.20 } else { 1.0 };

                        if stacked {
                            // StackedVP: ask bar as base (outermost, anchored to right edge),
                            // bid bar stacked on top of it (immediately to the left of ask bar).
                            let ask_bar_w = ((ask_qty_f32 / max_single_vp_qty) * vp_width).min(vp_width);
                            let bid_bar_w = ((bid_qty_f32 / max_single_vp_qty) * vp_width)
                                .min(vp_width - ask_bar_w); // clamp so combined never exceeds vp_width

                            // Ask bar: from right edge inward
                            if ask_bar_w > 0.0 {
                                frame.fill_rectangle(
                                    Point::new(cols.vp.1 - ask_bar_w, y),
                                    Size::new(ask_bar_w, bar_h),
                                    iced::Color { a: bar_alpha, ..ask_color },
                                );
                            }
                            // Bid bar: stacked on top (to the left) of the ask bar
                            if bid_bar_w > 0.0 {
                                frame.fill_rectangle(
                                    Point::new(cols.vp.1 - ask_bar_w - bid_bar_w, y),
                                    Size::new(bid_bar_w, bar_h),
                                    iced::Color { a: bar_alpha, ..bid_color },
                                );
                            }
                        } else {
                            // DeltaVP: single bar whose width represents |delta|,
                            // anchored to the right edge, coloured by dominant side.
                            let abs_delta = delta.abs();
                            let delta_bar_w = ((abs_delta / max_single_vp_qty) * vp_width).min(vp_width);
                            let bar_color = if delta >= 0.0 { bid_color } else { ask_color };
                            if delta_bar_w > 0.0 {
                                frame.fill_rectangle(
                                    Point::new(cols.vp.1 - delta_bar_w, y),
                                    Size::new(delta_bar_w, bar_h),
                                    iced::Color { a: bar_alpha, ..bar_color },
                                );
                            }
                        }

                        // Delta label — always white (text_color), same as order qty on DOM side
                        if self.config.show_trade_text && (bid_qty_f32 > 0.0 || ask_qty_f32 > 0.0) {
                            let delta_qty = Qty::from_f32(delta.abs());
                            let delta_str = if delta >= 0.0 {
                                format!("+{}", self.format_quantity(delta_qty))
                            } else {
                                format!("-{}", self.format_quantity(delta_qty))
                            };
                            Self::draw_cell_text(
                                frame,
                                &delta_str,
                                cols.vp.1 - 4.0,
                                y,
                                text_color,
                                Alignment::End,
                            );
                        }
                    }
                }
                DomRow::Spread => {
                    if let Some(spread) = self.raw_price_spread {
                        let min_ticksize = self.ticker_info.min_ticksize;
                        spread_row = Some((y, y + ROW_HEIGHT));
                        let spread = spread.round_to_min_tick(min_ticksize);
                        let content = format!("Spread: {}", spread.to_string(min_ticksize));
                        let price_x_center = (cols.price.0 + cols.price.1) * 0.5;
                        frame.fill_text(Text {
                            content,
                            position: Point::new(price_x_center, y + ROW_HEIGHT / 2.0),
                            color: text_color.scale_alpha(0.6),
                            size: (TEXT_SIZE - 1.0).into(),
                            font: style::AZERET_MONO,
                            align_x: Alignment::Center.into(),
                            align_y: Alignment::Center.into(),
                            ..Default::default()
                        });
                    }
                }
                DomRow::CenterDivider => {
                    let y_mid = y + ROW_HEIGHT / 2.0 - 0.5;
                    frame.fill_rectangle(
                        Point::new(0.0, y_mid),
                        Size::new(bounds.width, 1.0),
                        divider_color,
                    );
                }
            }
        }

        // Vertical dividers: identical to Original — just the two lines flanking the price column.
        // The VP panel needs no extra boundary line; it simply fills the space to the right.
        {
            let x = cols.order.1.floor() + 0.5;
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, bounds.height), divider_color);
        }
        {
            let x = cols.price.1.floor() + 0.5;
            frame.fill_rectangle(Point::new(x, 0.0), Size::new(1.0, bounds.height), divider_color);
        }
    }

    fn draw_row(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        y: f32,
        price: Price,
        order_qty: Qty,
        is_bid: bool,
        bid_color: iced::Color,
        ask_color: iced::Color,
        text_color: iced::Color,
        max_order_qty: f32,
        trade_buy_qty: Qty,
        trade_sell_qty: Qty,
        max_trade_qty: f32,
        trade_buy_color: iced::Color,
        trade_sell_color: iced::Color,
        cols: &ColumnRanges,
        outline_color: Option<iced::Color>,
        show_trade_text: bool,
    ) {
        let side_color = if is_bid { bid_color } else { ask_color };
        let order_qty_f32 = f32::from(order_qty);
        let trade_buy_qty_f32 = f32::from(trade_buy_qty);
        let trade_sell_qty_f32 = f32::from(trade_sell_qty);

        let order_alpha = if show_trade_text { 0.20 } else { 1.0 };
        let trade_alpha = if show_trade_text { 0.30 } else { 1.0 };
        let bar_h = ROW_HEIGHT - 1.0;

        // Order column: single shared column, bid bar below spread, ask bar above
        Self::fill_bar(
            frame,
            cols.order,
            y,
            bar_h,
            order_qty_f32,
            max_order_qty,
            side_color,
            true,   // grows rightward from left edge
            order_alpha,
            outline_color,
        );
        if show_trade_text {
            let qty_txt = self.format_quantity(order_qty);
            Self::draw_cell_text(frame, &qty_txt, cols.order.0 + 4.0, y, text_color, Alignment::Start);
        }

        // Ask (sell) trades: grow LEFTWARD from right_axis into the trade column
        Self::fill_bar(
            frame,
            cols.trade,
            y,
            bar_h,
            trade_sell_qty_f32,
            max_trade_qty,
            trade_sell_color,
            false,  // from_left=false → grows leftward from trade.1 = right_axis
            trade_alpha,
            outline_color,
        );
        // Bid (buy) trades: grow RIGHTWARD from right_axis to screen edge
        let bid_trade_col = (cols.right_axis, cols.right_axis + (cols.right_axis - cols.trade.0));
        Self::fill_bar(
            frame,
            bid_trade_col,
            y,
            bar_h,
            trade_buy_qty_f32,
            max_trade_qty,
            trade_buy_color,
            true,   // from_left=true → grows rightward from right_axis
            trade_alpha,
            outline_color,
        );

        // Text: ask text just left of axis, bid text just right of axis
        if show_trade_text {
            let sell_txt = self.format_quantity(trade_sell_qty);
            Self::draw_cell_text(
                frame, &sell_txt,
                cols.right_axis - 4.0, y,
                text_color, Alignment::End,
            );
            let buy_txt = self.format_quantity(trade_buy_qty);
            Self::draw_cell_text(
                frame, &buy_txt,
                cols.right_axis + 4.0, y,
                text_color, Alignment::Start,
            );
        }

        // Price
        let price_text = self.format_price(price);
        let price_x_center = (cols.price.0 + cols.price.1) * 0.5;
        Self::draw_cell_text(
            frame,
            &price_text,
            price_x_center,
            y,
            text_color,
            Alignment::Center,
        );
    }

    fn fill_bar(
        frame: &mut iced::widget::canvas::Frame,
        (x_start, x_end): (f32, f32),
        y: f32,
        height: f32,
        value: f32,
        scale_value_max: f32,
        color: iced::Color,
        from_left: bool,
        alpha: f32,
        outline_color: Option<iced::Color>,
    ) {
        if scale_value_max <= 0.0 || value <= 0.0 {
            return;
        }
        let col_width = x_end - x_start;

        let mut bar_width = (value / scale_value_max) * col_width.max(1.0);
        bar_width = bar_width.min(col_width);
        let bar_x = if from_left {
            x_start
        } else {
            x_end - bar_width
        };

        frame.fill_rectangle(
            Point::new(bar_x, y),
            Size::new(bar_width, height),
            iced::Color { a: alpha, ..color },
        );

        if let Some(outline) = outline_color {
            use iced::widget::canvas::{Path, Stroke};
            let stroke = Stroke::with_color(
                Stroke { width: 1.0, ..Default::default() },
                outline,
            );
            let top_left  = Point::new(bar_x, y);
            let top_right = Point::new(bar_x + bar_width, y);
            let bot_left  = Point::new(bar_x, y + height);
            let bot_right = Point::new(bar_x + bar_width, y + height);

            if from_left {
                let path = Path::new(|b| {
                    b.move_to(top_left);
                    b.line_to(top_right);
                    b.line_to(bot_right);
                });
                frame.stroke(&path, stroke);
            } else {
                let path = Path::new(|b| {
                    b.move_to(top_right);
                    b.line_to(top_left);
                    b.line_to(bot_left);
                });
                frame.stroke(&path, stroke);
            }
        }
    }

    fn draw_cell_text(
        frame: &mut iced::widget::canvas::Frame,
        text: &str,
        x_anchor: f32,
        y: f32,
        color: iced::Color,
        align: Alignment,
    ) {
        frame.fill_text(Text {
            content: text.to_string(),
            position: Point::new(x_anchor, y + ROW_HEIGHT / 2.0),
            color,
            size: TEXT_SIZE.into(),
            font: style::AZERET_MONO,
            align_x: align.into(),
            align_y: Alignment::Center.into(),
            ..Default::default()
        });
    }

    fn draw_chase_trail(
        &self,
        frame: &mut iced::widget::canvas::Frame,
        grid: &PriceGrid,
        bounds: Rectangle,
        tracker: &ChaseTracker,
        pos_x: f32,
        best_offer_y: Option<f32>,
        color: iced::Color,
        is_bid: bool,
    ) {
        let radius = CHASE_CIRCLE_RADIUS;
        if let Some((start_p_raw, end_p_raw, alpha)) = tracker.segment() {
            let start_p = start_p_raw.round_to_side_step(is_bid, grid.tick);
            let end_p = end_p_raw.round_to_side_step(is_bid, grid.tick);

            let color = color.scale_alpha(alpha);
            let stroke_w = 2.0;
            let pad_to_circle = radius + stroke_w * 0.5;

            let start_y = self.price_to_screen_y(start_p, grid, bounds.height);
            let end_y = self
                .price_to_screen_y(end_p, grid, bounds.height)
                .or(best_offer_y);

            if let Some(end_y) = end_y {
                if let Some(start_y) = start_y {
                    let dy = end_y - start_y;
                    if dy.abs() > pad_to_circle {
                        let line_end_y = end_y - dy.signum() * pad_to_circle;
                        let line_path =
                            Path::line(Point::new(pos_x, start_y), Point::new(pos_x, line_end_y));
                        frame.stroke(
                            &line_path,
                            Stroke::default().with_color(color).with_width(stroke_w),
                        );
                    }
                }

                let circle = &Path::circle(Point::new(pos_x, end_y), radius);
                frame.fill(circle, color);
            }
        }
    }

    fn build_price_grid(&self) -> Option<PriceGrid> {
        let best_bid = match (self.best_price(Side::Bid), self.best_price(Side::Ask)) {
            (Some(bb), _) => bb,
            (None, Some(ba)) => ba.add_steps(-1, self.step),
            (None, None) => {
                let (min_t, max_t) = self.trades.price_range()?;
                let steps = Price::steps_between_inclusive(min_t, max_t, self.step).unwrap_or(1);
                max_t.add_steps(-(steps as i64 / 2), self.step)
            }
        };
        let best_ask = best_bid.add_steps(1, self.step);

        Some(PriceGrid {
            best_bid,
            best_ask,
            tick: self.step,
        })
    }

    fn visible_rows(&self, bounds: Rectangle, grid: &PriceGrid) -> (Vec<VisibleRow>, Maxima) {
        let asks_grouped = self.grouped_asks();
        let bids_grouped = self.grouped_bids();

        let mut visible: Vec<VisibleRow> = Vec::new();
        let mut maxima = Maxima::default();

        let mid_screen_y = bounds.height * 0.5;
        let scroll = self.scroll_px;

        let y0 = mid_screen_y + PriceGrid::top_y(0) - scroll;
        let idx_top = ((0.0 - y0) / ROW_HEIGHT).floor() as i32;

        let rows_needed = (bounds.height / ROW_HEIGHT).ceil() as i32 + 1;
        let idx_bottom = idx_top + rows_needed;

        for idx in idx_top..=idx_bottom {
            if idx == 0 {
                let top_y_screen = mid_screen_y + PriceGrid::top_y(0) - scroll;
                if top_y_screen < bounds.height && top_y_screen + ROW_HEIGHT > 0.0 {
                    let row = if self.config.show_spread
                        && self.ticker_info.exchange().is_depth_client_aggr()
                    {
                        DomRow::Spread
                    } else {
                        DomRow::CenterDivider
                    };

                    visible.push(VisibleRow {
                        row,
                        y: top_y_screen,
                        buy_t: Qty::default(),
                        sell_t: Qty::default(),
                    });
                }
                continue;
            }

            let Some(price) = grid.index_to_price(idx) else {
                continue;
            };

            let is_bid = idx > 0;
            let order_qty = if is_bid {
                bids_grouped.get(&price).copied().unwrap_or_default()
            } else {
                asks_grouped.get(&price).copied().unwrap_or_default()
            };

            let top_y_screen = mid_screen_y + PriceGrid::top_y(idx) - scroll;
            if top_y_screen >= bounds.height || top_y_screen + ROW_HEIGHT <= 0.0 {
                continue;
            }

            maxima.vis_max_order_qty = maxima.vis_max_order_qty.max(f32::from(order_qty));
            let (buy_t, sell_t) = self.trade_qty_at(price);
            maxima.vis_max_trade_qty = maxima
                .vis_max_trade_qty
                .max(f32::from(buy_t).max(f32::from(sell_t)));

            let row = if is_bid {
                DomRow::Bid {
                    price,
                    qty: order_qty,
                }
            } else {
                DomRow::Ask {
                    price,
                    qty: order_qty,
                }
            };

            visible.push(VisibleRow {
                row,
                y: top_y_screen,
                buy_t,
                sell_t,
            });
        }

        visible.sort_by(|a, b| a.y.total_cmp(&b.y));

        // Cumulative trades: accumulate from price extremes toward spread
        if self.config.cumulative_trades {
            let split = visible.iter().position(|r| {
                matches!(r.row, DomRow::Spread | DomRow::CenterDivider)
            });
            if let Some(split_idx) = split {
                let mut cum_buy = Qty::default();
                let mut cum_sell = Qty::default();
                for row in visible[..split_idx].iter_mut().rev() {
                    if matches!(row.row, DomRow::Ask { .. }) {
                        cum_buy += row.buy_t;
                        cum_sell += row.sell_t;
                        row.buy_t = cum_buy;
                        row.sell_t = cum_sell;
                    }
                }
                let mut cum_buy = Qty::default();
                let mut cum_sell = Qty::default();
                for row in visible[split_idx + 1..].iter_mut() {
                    if matches!(row.row, DomRow::Bid { .. }) {
                        cum_buy += row.buy_t;
                        cum_sell += row.sell_t;
                        row.buy_t = cum_buy;
                        row.sell_t = cum_sell;
                    }
                }
                maxima.vis_max_trade_qty = visible.iter()
                    .map(|r| f32::from(r.buy_t).max(f32::from(r.sell_t)))
                    .fold(0.0_f32, f32::max);
            }
        }

        // Cumulative orders
        if self.config.cumulative_orders {
            let split = visible.iter().position(|r| {
                matches!(r.row, DomRow::Spread | DomRow::CenterDivider)
            });
            if let Some(split_idx) = split {
                let mut cum_ask = Qty::default();
                for row in visible[..split_idx].iter_mut().rev() {
                    if let DomRow::Ask { ref mut qty, .. } = row.row {
                        cum_ask += *qty;
                        *qty = cum_ask;
                    }
                }
                let mut cum_bid = Qty::default();
                for row in visible[split_idx + 1..].iter_mut() {
                    if let DomRow::Bid { ref mut qty, .. } = row.row {
                        cum_bid += *qty;
                        *qty = cum_bid;
                    }
                }
                maxima.vis_max_order_qty = visible.iter()
                    .map(|r| match &r.row {
                        DomRow::Ask { qty, .. } | DomRow::Bid { qty, .. } => f32::from(*qty),
                        _ => 0.0,
                    })
                    .fold(0.0_f32, f32::max);
            }
        }

        (visible, maxima)
    }

    fn price_to_screen_y(&self, price: Price, grid: &PriceGrid, bounds_height: f32) -> Option<f32> {
        let mid_screen_y = bounds_height * 0.5;
        let scroll = self.scroll_px;

        let idx = if price >= grid.best_ask {
            let steps = Price::steps_between_inclusive(grid.best_ask, price, grid.tick)?;
            -(steps as i32)
        } else if price <= grid.best_bid {
            let steps = Price::steps_between_inclusive(price, grid.best_bid, grid.tick)?;
            steps as i32
        } else {
            return Some(mid_screen_y - scroll);
        };

        let y = mid_screen_y + PriceGrid::top_y(idx) - scroll + ROW_HEIGHT / 2.0;
        Some(y)
    }
}

enum DomRow {
    Ask { price: Price, qty: Qty },
    Spread,
    CenterDivider,
    Bid { price: Price, qty: Qty },
}

struct PriceGrid {
    best_bid: Price,
    best_ask: Price,
    tick: PriceStep,
}

impl PriceGrid {
    /// Returns None for index 0 (spread row)
    fn index_to_price(&self, idx: i32) -> Option<Price> {
        if idx == 0 {
            return None;
        }
        if idx > 0 {
            let off = (idx - 1) as i64; // 1 => best_bid, 2 => best_bid - 1 tick
            Some(self.best_bid.add_steps(-off, self.tick))
        } else {
            let off = (-1 - idx) as i64; // -1 => best_ask, -2 => best_ask + 1 tick
            Some(self.best_ask.add_steps(off, self.tick))
        }
    }

    fn top_y(idx: i32) -> f32 {
        (idx as f32) * ROW_HEIGHT - ROW_HEIGHT * 0.5
    }
}
