use super::*;
use crate::history_cell::PlainHistoryCell;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseEventKind::{Down, Drag, Up};
use pretty_assertions::assert_eq;

const BODY: Rect = Rect::new(2, 1, 20, 8);
const RAIL: u16 = BODY.x + BODY.width;

fn message(label: &str, rows: usize) -> Arc<dyn HistoryCell> {
    Arc::new(PlainHistoryCell::new(
        (0..rows)
            .map(|i| format!("{label} {i:03}").into())
            .collect(),
    ))
}

fn paint(view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], column: Option<u16>) -> Buffer {
    let mut buf = Buffer::empty(Rect::new(0, 0, RAIL + 1, BODY.bottom()));
    for _ in 0..100 {
        view.render(BODY, &mut buf, cells);
        if !view.paint_scrollbar(column, &mut buf, cells) {
            return buf;
        }
    }
    panic!("scrollbar measurement did not finish");
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn text(buf: &Buffer) -> Vec<String> {
    (BODY.y..BODY.bottom())
        .map(|row| {
            (BODY.x..BODY.right())
                .map(|x| buf[(x, row)].symbol())
                .collect()
        })
        .collect()
}

#[test]
fn pointer_drags_through_one_tall_message_and_returns_to_following() {
    let cells = vec![message("row", 120)];
    let mut view = TranscriptView::default();
    let buf = paint(&mut view, &cells, Some(RAIL));
    let bottom = BODY.bottom() - 1;
    assert_eq!(buf[(RAIL, bottom)].symbol(), "█");
    let middle = BODY.y + BODY.height / 2;
    let left = MouseButton::Left;
    assert!(matches!(
        view.handle_scrollbar_mouse(mouse(Down(left), RAIL, bottom), &cells),
        Some(ScrollbarAction::Handled)
    ));
    view.handle_scrollbar_mouse(mouse(Drag(left), RAIL, BODY.y), &cells);
    assert!(text(&paint(&mut view, &cells, Some(RAIL)))[0].contains("row 000"));
    // With no older page to load, the same gesture can reverse away from the top.
    // The mouse can leave the rail (or the pane) during a captured drag.
    view.handle_scrollbar_mouse(mouse(Drag(left), 0, middle), &cells);
    let mid = paint(&mut view, &cells, Some(RAIL));
    let first = text(&mid)[0]
        .trim()
        .strip_prefix("row ")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    assert!((40..85).contains(&first));
    assert_eq!(mid[(RAIL, middle)].symbol(), "█");
    view.handle_scrollbar_mouse(mouse(Up(left), 0, bottom + 1), &cells);
    assert!(view.is_following());
    assert!(
        view.handle_scrollbar_mouse(mouse(Drag(left), RAIL, middle), &cells)
            .is_none()
    );
}

#[test]
fn reaching_top_loads_one_page_and_keeps_text_under_the_pointer() {
    let mut cells = vec![message("retained", 64)];
    let mut view = TranscriptView {
        history: TranscriptHistoryState::Partial,
        ..Default::default()
    };
    paint(&mut view, &cells, Some(RAIL));
    let left = MouseButton::Left;
    view.handle_scrollbar_mouse(mouse(Down(left), RAIL, BODY.bottom() - 1), &cells);
    assert!(matches!(
        view.handle_scrollbar_mouse(mouse(Drag(left), RAIL, BODY.y), &cells),
        Some(ScrollbarAction::LoadOlder)
    ));
    let before = text(&paint(&mut view, &cells, Some(RAIL)));
    assert!(before[0].contains("retained 000"));
    cells.insert(
        0,
        Arc::new(crate::history_cell::new_user_prompt(
            "earlier prompt".into(),
            vec![],
            vec![],
            vec![],
        )),
    );
    view.history_loaded(&cells, 0..1);
    assert_eq!(text(&paint(&mut view, &cells, Some(RAIL))), before);
    assert!(!view.needs_history(&cells));
    for kind in [Drag(left), Up(left)] {
        assert!(matches!(
            view.handle_scrollbar_mouse(mouse(kind, RAIL, BODY.y), &cells),
            Some(ScrollbarAction::Handled)
        ));
        assert_eq!(text(&paint(&mut view, &cells, Some(RAIL))), before);
    }
    // A new track click reaches the newly loaded page.
    let track = view.scrollbar.track.unwrap();
    let y = track.area.y + track.thumb_top;
    view.handle_scrollbar_mouse(mouse(Down(left), RAIL, y), &cells);
    view.handle_scrollbar_mouse(mouse(Drag(left), RAIL, BODY.y.saturating_sub(1)), &cells);
    let new_page = text(&paint(&mut view, &cells, Some(RAIL)));
    assert_eq!(view.start(&cells).0, 0, "{new_page:?}");
    // User prompts include a top padding row in the transcript.
    let first_text = new_page
        .iter()
        .find(|line| !line.trim().is_empty())
        .unwrap();
    assert!(first_text.contains("earlier prompt"), "{new_page:?}");
}

#[test]
fn fitting_and_disabled_scrollbars_leave_mouse_events_to_the_transcript() {
    let cells = vec![message("short", 2)];
    let mut view = TranscriptView::default();
    for (cells, column) in [(cells, Some(RAIL)), (vec![message("long", 100)], None)] {
        let buf = paint(&mut view, &cells, column);
        assert_eq!(buf[(RAIL, BODY.y)].symbol(), " ");
        assert!(
            view.handle_scrollbar_mouse(mouse(Down(MouseButton::Left), RAIL, BODY.y), &cells)
                .is_none()
        );
    }
}
