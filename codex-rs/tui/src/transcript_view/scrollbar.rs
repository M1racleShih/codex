//! Mouse navigation on the reserved right edge of the fullscreen conversation.

use super::*;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;

#[derive(Clone, Copy)]
struct Track {
    area: Rect,
    thumb_top: u16,
    thumb_len: u16,
    max_offset: usize,
}

fn scale(value: usize, from: usize, to: usize) -> usize {
    if from == 0 {
        return 0;
    }
    ((value as u128 * to as u128 + from as u128 / 2) / from as u128) as usize
}

impl Track {
    fn new(area: Rect, total: usize, viewport: usize, offset: usize, following: bool) -> Self {
        let height = usize::from(area.height);
        let thumb_len = scale(viewport.min(total), total, height).clamp(1, height - 1) as u16;
        let max_offset = total.saturating_sub(viewport);
        let travel = area.height - thumb_len;
        let thumb_top = if following {
            travel
        } else {
            scale(offset.min(max_offset), max_offset, usize::from(travel)) as u16
        };
        Self {
            area,
            thumb_top,
            thumb_len,
            max_offset,
        }
    }
}

#[derive(Clone, Copy)]
struct ThumbDrag {
    grab: u16,
    last_y: u16,
    // After prepending, the same pointer position maps to different text. Finish this
    // gesture at the existing anchor and pick up the new range on the next press.
    reached_top: bool,
}

#[derive(Default)]
pub(super) struct Scrollbar {
    track: Option<Track>,
    drag: Option<ThumbDrag>,
}

pub(crate) enum ScrollbarAction {
    Handled,
    LoadOlder,
}

impl TranscriptView {
    pub(crate) fn clear_scrollbar(&mut self) {
        self.scrollbar = Scrollbar::default();
    }

    /// Returns true while the row index needs more measurement frames.
    pub(crate) fn paint_scrollbar(
        &mut self,
        column: Option<u16>,
        buf: &mut Buffer,
        cells: &[Arc<dyn HistoryCell>],
    ) -> bool {
        let Some(x) = column.filter(|_| self.area.height >= 2 && !self.area.is_empty()) else {
            self.scrollbar = Scrollbar::default();
            return false;
        };
        let pending = self.update_scrollbar_navigation(cells);
        let Some(metrics) = self.scrollbar_metrics() else {
            self.scrollbar.track = None;
            return pending;
        };
        if metrics.total_rows <= metrics.viewport_rows && !self.history.has_unloaded_history() {
            self.scrollbar = Scrollbar::default();
            return pending;
        }
        let area = Rect::new(x, self.area.y, 1, self.area.height);
        let track = Track::new(
            area,
            metrics.total_rows,
            metrics.viewport_rows,
            metrics.offset,
            self.is_following(),
        );
        for row in 0..area.height {
            let cell = &mut buf[(x, area.y + row)];
            if (track.thumb_top..track.thumb_top + track.thumb_len).contains(&row) {
                cell.set_symbol("┃").set_style(
                    crate::style::user_message_style()
                        .fg(crate::style::user_message_accent_color()),
                );
            } else {
                cell.set_symbol("│")
                    .set_style(crate::style::secondary_text_style());
            }
        }
        self.scrollbar.track = Some(track);
        pending
    }

    pub(crate) fn handle_scrollbar_mouse(
        &mut self,
        event: MouseEvent,
        cells: &[Arc<dyn HistoryCell>],
    ) -> Option<ScrollbarAction> {
        use MouseEventKind::*;
        let left = MouseButton::Left;
        if matches!(event.kind, Down(_) | Moved | ScrollUp | ScrollDown) {
            self.scrollbar.drag = None;
        }
        let track = self.scrollbar.track;
        let inside = track.is_some_and(|t| t.area.contains((event.column, event.row).into()));
        match event.kind {
            Down(button) if button == left && inside && event.modifiers.is_empty() => {
                let track = track?;
                let row = event.row - track.area.y;
                let on_thumb = (track.thumb_top..track.thumb_top + track.thumb_len).contains(&row);
                self.scrollbar.drag = Some(ThumbDrag {
                    grab: if on_thumb {
                        row - track.thumb_top
                    } else {
                        track.thumb_len / 2
                    },
                    last_y: event.row,
                    reached_top: false,
                });
                if on_thumb && row != 0 {
                    return Some(ScrollbarAction::Handled);
                }
            }
            Drag(button) | Up(button) if button == left && self.scrollbar.drag.is_some() => {
                let drag = self.scrollbar.drag?;
                if drag.reached_top || drag.last_y == event.row || track.is_none() {
                    if matches!(event.kind, Up(_)) {
                        self.scrollbar.drag = None;
                    }
                    return Some(ScrollbarAction::Handled);
                }
            }
            ScrollUp | ScrollDown if inside => {
                // The rail occupies its own column. Forward wheel input to the adjacent body.
                let body_event = MouseEvent {
                    column: self.area.right().saturating_sub(1),
                    ..event
                };
                self.handle_mouse(body_event, cells)?;
                return Some(ScrollbarAction::Handled);
            }
            _ => return None,
        }

        let track = track?;
        let drag = self.scrollbar.drag.as_mut()?;
        let travel = track.area.height - track.thumb_len;
        let row = event
            .row
            .saturating_sub(track.area.y)
            .saturating_sub(drag.grab)
            .min(travel);
        drag.last_y = event.row;
        if matches!(event.kind, Up(_)) {
            self.scrollbar.drag = None;
        }
        if row == travel {
            self.jump_to_latest();
        } else {
            let offset = scale(usize::from(row), usize::from(travel), track.max_offset);
            if self.seek_scrollbar(cells, offset, row == 0) {
                if let Some(drag) = &mut self.scrollbar.drag {
                    drag.reached_top = true;
                }
                return Some(ScrollbarAction::LoadOlder);
            }
        }
        Some(ScrollbarAction::Handled)
    }
}

#[cfg(test)]
#[path = "scrollbar_tests.rs"]
mod tests;
