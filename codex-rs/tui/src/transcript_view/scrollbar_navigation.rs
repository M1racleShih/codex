//! Index rendered row counts for absolute navigation through the loaded conversation.

use std::ops::Range;
use std::time::Duration;
use std::time::Instant;

use crate::history_cell::ClearScreenHeader;
use crate::history_cell::DynamicToolCallCell;
use crate::history_cell::ThreadRecapHistoryCell;

use super::*;

const ENTRIES_PER_FRAME: usize = 64;
const MEASUREMENT_BUDGET: Duration = Duration::from_millis(2);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScrollbarMetrics {
    pub(crate) total_rows: usize,
    pub(crate) viewport_rows: usize,
    pub(crate) offset: usize,
}

#[derive(Clone, Copy, Default, Eq, PartialEq)]
struct EntryHeight {
    rows: usize,
    separated: bool,
}

impl From<&TextLayout> for EntryHeight {
    fn from(layout: &TextLayout) -> Self {
        Self {
            rows: layout.row_count(),
            separated: layout.separated,
        }
    }
}

/// Keep measured heights and the prefix sums still valid after changes to those heights.
#[derive(Default)]
struct RowIndex {
    entries: Vec<Option<EntryHeight>>,
    ends: Vec<usize>,
    next_unknown: usize,
}

impl RowIndex {
    fn resize(&mut self, count: usize) {
        self.entries.resize(count, None);
        self.ends.truncate(count);
        self.next_unknown = self.next_unknown.min(count);
    }

    fn set(&mut self, index: usize, height: Option<EntryHeight>) {
        if self.entries[index] != height {
            self.entries[index] = height;
            self.ends.truncate(index);
        }
    }

    fn invalidate(&mut self, range: Range<usize>) {
        let end = range.end.min(self.entries.len());
        if range.start < end {
            self.entries[range.start..end].fill(None);
            self.ends.truncate(range.start);
            self.next_unknown = self.next_unknown.min(range.start);
        }
    }

    fn next_unknown(&mut self) -> Option<usize> {
        let next = self.entries[self.next_unknown..]
            .iter()
            .position(Option::is_none)
            .map(|index| index + self.next_unknown);
        self.next_unknown = next.unwrap_or(self.entries.len());
        next
    }

    fn rebuild_ends(&mut self) {
        debug_assert_eq!(self.next_unknown, self.entries.len());
        let mut end = self.ends.last().copied().unwrap_or(0);
        for height in self.entries[self.ends.len()..].iter().flatten() {
            end += height.rows;
            self.ends.push(end);
        }
    }

    fn rows_before(&self, index: usize) -> usize {
        index
            .checked_sub(1)
            .map_or(0, |previous| self.ends[previous])
    }

    fn row_at(&self, offset: usize) -> Option<(usize, usize)> {
        let index = self.ends.partition_point(|end| *end <= offset);
        (index < self.ends.len()).then(|| (index, offset - self.rows_before(index)))
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Presentation {
    width: u16,
    detailed: bool,
    mode: HistoryRenderMode,
    collapse_turn_details: bool,
    hide_session_info: bool,
    image_protocol: Option<crate::pets::ImageProtocol>,
    render_revision: u64,
    snapshot: Option<usize>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct Source {
    len: usize,
    first: Option<EntryKey>,
    last: Option<EntryKey>,
}

#[derive(Default)]
pub(super) struct ScrollbarNavigation {
    presentation: Option<Presentation>,
    source: Option<Source>,
    first_cell: usize,
    clear_screen: bool,
    rows: RowIndex,
    live: Option<EntryHeight>,
    skip_first_row: usize,
    invalidated_suffix: Option<usize>,
    active_tools: Vec<usize>,
    turn_tip: Option<EntryKey>,
    search_match: Option<(EntryKey, usize, usize)>,
    metrics: Option<ScrollbarMetrics>,
    hold_viewport_paging: bool,
    unheaded_anchor: Option<Anchor>,
}

impl ScrollbarNavigation {
    fn invalidate(&mut self, range: Range<usize>) {
        self.rows.invalidate(
            range.start.saturating_sub(self.first_cell)..range.end.saturating_sub(self.first_cell),
        );
        self.metrics = None;
    }

    fn sync_source(&mut self, cells: &[Arc<dyn HistoryCell>], presentation: Presentation) {
        let source = Source {
            len: cells.len(),
            first: cells.first().map(EntryKey::cell),
            last: cells.last().map(EntryKey::cell),
        };
        let previous = self.source;
        let invalidated_suffix = self.invalidated_suffix.take();
        let can_append = previous.is_some_and(|previous| {
            previous.len <= source.len
                && (previous.len == 0
                    || (previous.first == source.first
                        && cells.get(previous.len - 1).map(EntryKey::cell) == previous.last))
        });
        let changed = self.presentation != Some(presentation);
        if changed || previous != Some(source) {
            let old_start = self.first_cell;
            if changed || !can_append {
                self.clear_screen = false;
                self.first_cell = 0;
            }
            let search_from = if changed || !can_append {
                0
            } else {
                previous.map_or(0, |previous| previous.len)
            };
            if !presentation.detailed
                && let Some(index) = cells[search_from..]
                    .iter()
                    .rposition(|cell| cell.as_any().is::<ClearScreenHeader>())
            {
                self.clear_screen = true;
                self.first_cell = search_from + index;
            }
            let known_suffix_change = invalidated_suffix.is_some_and(|index| {
                index > old_start
                    && index <= source.len
                    && previous.is_some_and(|previous| previous.first == source.first)
            });
            if changed || (!can_append && !known_suffix_change) || self.first_cell != old_start {
                self.rows = RowIndex::default();
                self.active_tools.clear();
            }
            self.rows.resize(cells.len() - self.first_cell);
            self.presentation = Some(presentation);
            self.source = Some(source);
        }
    }

    fn record(&mut self, index: usize, cell: &dyn HistoryCell, height: EntryHeight) {
        if index >= self.first_cell {
            if is_pending_tool(cell) && !self.active_tools.contains(&index) {
                self.active_tools.push(index);
            }
            self.rows.set(index - self.first_cell, Some(height));
        }
    }

    fn invalidate_finished_tools(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        self.active_tools.retain(|&index| {
            if cells
                .get(index)
                .is_some_and(|cell| is_pending_tool(cell.as_ref()))
            {
                return true;
            }
            if index >= self.first_cell {
                let relative = index - self.first_cell;
                self.rows.invalidate(relative..relative + 1);
            }
            false
        });
    }

    fn measure_entries(
        &mut self,
        view: &mut TranscriptView,
        cells: &[Arc<dyn HistoryCell>],
    ) -> bool {
        let mut pending = self.rows.next_unknown();
        if pending.is_none() {
            return false;
        }

        // Measuring scrolled-out entries must not evict cached visible layouts. Reuse the normal
        // line layout in a scratch cache and keep just the row counts between frames.
        let viewport_cache = std::mem::replace(&mut view.cache, LayoutCache::for_measurement());
        let started = Instant::now();
        let mut measured = 0;
        while let Some(relative) = pending
            && measured < ENTRIES_PER_FRAME
            && (measured == 0 || started.elapsed() < MEASUREMENT_BUDGET)
        {
            let index = relative + self.first_cell;
            let height = view
                .layout(cells, index)
                .as_deref()
                .map(EntryHeight::from)
                .unwrap_or_default();
            self.record(index, cells[index].as_ref(), height);
            measured += 1;
            pending = self.rows.next_unknown();
        }
        view.cache = viewport_cache;
        pending.is_some()
    }

    fn first_height(&self) -> Option<EntryHeight> {
        self.rows
            .row_at(0)
            .and_then(|(index, _)| self.rows.entries[index])
            .or(self.live)
    }

    fn row_at(&self, offset: usize) -> (usize, usize) {
        let offset = offset.saturating_add(self.skip_first_row);
        if let Some((index, row)) = self.rows.row_at(offset) {
            return (index + self.first_cell, row);
        }
        let end = self.rows.entries.len();
        let row = offset
            .saturating_sub(self.rows.rows_before(end))
            .min(self.live.map_or(0, |height| height.rows.saturating_sub(1)));
        (end + self.first_cell, row)
    }
}

fn is_pending_tool(cell: &dyn HistoryCell) -> bool {
    cell.as_any()
        .downcast_ref::<DynamicToolCallCell>()
        .is_some_and(DynamicToolCallCell::is_active)
}

impl TranscriptView {
    pub(super) fn invalidate_scrollbar_from(&mut self, start: usize) {
        let navigation = &mut self.scrollbar_navigation;
        navigation.invalidated_suffix = Some(
            navigation
                .invalidated_suffix
                .map_or(start, |old| old.min(start)),
        );
        self.invalidate_scrollbar_range(start..usize::MAX);
    }

    pub(super) fn invalidate_scrollbar_range(&mut self, range: Range<usize>) {
        self.scrollbar_navigation.invalidate(range);
    }

    pub(super) fn scrollbar_holds_viewport_paging(&self) -> bool {
        self.scrollbar_navigation.hold_viewport_paging
    }

    pub(super) fn release_scrollbar_paging(&mut self) {
        self.scrollbar_navigation.hold_viewport_paging = false;
        self.scrollbar_navigation.unheaded_anchor = None;
    }

    /// A prompt arriving on an older page must not insert a sticky header above the
    /// answer being read. A prepend can change the entry index while keeping its key.
    pub(super) fn scrollbar_keeps_header_hidden(&self) -> bool {
        matches!((self.scrollbar_navigation.unheaded_anchor, self.position),
            (Some(old), Position::Reading(now)) if old == Anchor { index: old.index, ..now })
    }

    pub(crate) fn scrollbar_metrics(&self) -> Option<ScrollbarMetrics> {
        self.scrollbar_navigation.metrics
    }

    /// Update the row index after the final transcript paint. Return true while unmeasured
    /// entries remain, so callers can schedule another bounded batch.
    pub(crate) fn update_scrollbar_navigation(&mut self, cells: &[Arc<dyn HistoryCell>]) -> bool {
        let mut navigation = std::mem::take(&mut self.scrollbar_navigation);
        let pending = self.update_scrollbar_rows(cells, &mut navigation);
        self.scrollbar_navigation = navigation;
        pending
    }

    fn update_scrollbar_rows(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        navigation: &mut ScrollbarNavigation,
    ) -> bool {
        navigation.metrics = None;
        if self.area.is_empty() {
            return false;
        }
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        navigation.sync_source(
            cells,
            Presentation {
                width: self.area.width,
                detailed: self.detailed,
                mode: self.mode,
                collapse_turn_details: self.turn_details.is_some(),
                hide_session_info: self.history.has_unloaded_history(),
                image_protocol: self.image_protocol,
                render_revision: self.cache.render_revision(),
                snapshot: snapshot.as_deref().map(|cells| cells.as_ptr() as usize),
            },
        );

        if navigation.turn_tip != self.turn_tip_key {
            for key in [navigation.turn_tip, self.turn_tip_key]
                .into_iter()
                .flatten()
            {
                if let Some(index) = cells.iter().rposition(|cell| EntryKey::cell(cell) == key) {
                    navigation.invalidate(index..index + 1);
                }
            }
            navigation.turn_tip = self.turn_tip_key;
        }
        let matched = self.search.match_anchor().and_then(|anchor| {
            let layout = self.search.match_layout(anchor.key)?;
            Some((anchor.key, self.resolve(cells, anchor), layout.row_count()))
        });
        if navigation.search_match != matched {
            if let Some((key, index, _)) = navigation.search_match
                && cells.get(index).map(EntryKey::cell) == Some(key)
            {
                navigation.invalidate(index..index + 1);
            }
            if let Some((_, index, _)) = matched {
                navigation.invalidate(index..index + 1);
            }
            navigation.search_match = matched;
        }
        navigation.invalidate_finished_tools(cells);
        for group in self
            .visible
            .chunk_by(|left, right| left.index == right.index)
        {
            let visible = &group[0];
            if let Some(cell) = cells.get(visible.index) {
                navigation.record(visible.index, cell.as_ref(), visible.layout.as_ref().into());
            }
        }
        navigation.live = self
            .layout(cells, cells.len())
            .as_deref()
            .map(EntryHeight::from);
        if navigation.measure_entries(self, cells) {
            return true;
        }

        navigation.rows.rebuild_ends();
        let end = navigation.rows.entries.len();
        navigation.skip_first_row = usize::from(
            !navigation.clear_screen
                && navigation
                    .first_height()
                    .is_some_and(|height| height.separated),
        );
        let viewport_rows = usize::from(self.area.height);
        let recap_gap = usize::from(
            viewport_rows > 1
                && navigation.live.is_none()
                && cells
                    .last()
                    .is_some_and(|cell| cell.as_any().is::<ThreadRecapHistoryCell>()),
        );
        let total_rows = (navigation.rows.rows_before(end)
            + navigation.live.map_or(0, |height| height.rows)
            + recap_gap)
            .saturating_sub(navigation.skip_first_row);
        let last_start = total_rows.saturating_sub(viewport_rows);
        let offset = if self.position == Position::Latest {
            last_start
        } else {
            self.visible.first().map_or(0, |visible| {
                let index = visible.index.saturating_sub(navigation.first_cell).min(end);
                (navigation.rows.rows_before(index) + visible.row)
                    .saturating_sub(navigation.skip_first_row)
                    .min(last_start)
            })
        };
        navigation.metrics = Some(ScrollbarMetrics {
            total_rows,
            viewport_rows,
            offset,
        });
        false
    }

    /// Seek to a scrollbar row. Return true when hitting the start of a partially loaded history.
    pub(crate) fn seek_scrollbar(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        offset: usize,
        at_top: bool,
    ) -> bool {
        let navigation = &self.scrollbar_navigation;
        let Some(metrics) = navigation.metrics else {
            return false;
        };
        let request_older =
            at_top && !navigation.clear_screen && self.history.has_unloaded_history();
        let offset = if at_top { 0 } else { offset };
        let target =
            navigation.row_at(offset.min(metrics.total_rows.saturating_sub(metrics.viewport_rows)));
        self.disclosure.resume_after_approval = None;
        self.last_click = None;
        self.pending_mouse_scroll = 0.0;
        self.cancel_beginning();
        if let Some(selection) = &mut self.selection {
            selection.resume_on_empty = false;
            selection.pointer = None;
        }
        let snapshot = self.snapshot_cells();
        let displayed = snapshot.as_deref().unwrap_or(cells);
        if let Some(layout) = self.layout(displayed, target.0)
            && layout.row_count() > 0
        {
            let row = target.1.min(layout.row_count() - 1);
            self.read_from_row(displayed, target.0, row, &layout);
            self.hold_live_reading(cells, layout);
        }
        self.scrollbar_navigation.unheaded_anchor = match self.position {
            Position::Reading(anchor)
                if request_older
                    && prompt_header::line(displayed, target.0, self.area.width, self.detailed)
                        .is_none() =>
            {
                Some(anchor)
            }
            _ => None,
        };
        self.scrollbar_navigation.hold_viewport_paging = true;
        request_older
    }
}

#[cfg(test)]
#[path = "scrollbar_navigation_tests.rs"]
mod tests;
