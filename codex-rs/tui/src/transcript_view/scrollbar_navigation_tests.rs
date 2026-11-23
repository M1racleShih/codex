use pretty_assertions::assert_eq;

use super::super::tests::cell;
use super::super::tests::render;
use super::*;

fn index_rows(
    view: &mut TranscriptView,
    cells: &[Arc<dyn HistoryCell>],
    width: u16,
    height: u16,
) -> ScrollbarMetrics {
    render(view, cells, width, height);
    for _ in 0..=cells.len() {
        if !view.update_scrollbar_navigation(cells) {
            return view.scrollbar_metrics().expect("rows indexed");
        }
    }
    panic!("row index did not finish")
}

#[test]
fn scrollbar_locates_rows_inside_one_long_entry() {
    let cells = [cell(
        (0..300)
            .map(|row| format!("line {row}\n"))
            .collect::<String>(),
    )];
    let mut view = TranscriptView::default();
    assert_eq!(
        index_rows(&mut view, &cells, /*width*/ 40, /*height*/ 8),
        ScrollbarMetrics {
            total_rows: 300,
            viewport_rows: 8,
            offset: 292,
        }
    );

    assert!(!view.seek_scrollbar(&cells, /*offset*/ 163, /*at_top*/ false));
    let metrics = index_rows(&mut view, &cells, /*width*/ 40, /*height*/ 8);
    assert_eq!(metrics.offset, 163);
    assert_eq!(
        view.visible.first().map(|row| (row.index, row.row)),
        Some((0, 163))
    );

    view.jump_to_latest();
    assert_eq!(
        index_rows(&mut view, &cells, /*width*/ 40, /*height*/ 8).offset,
        292
    );
    assert!(view.is_following());
}

#[test]
fn scrollbar_top_holds_the_anchor_after_a_short_page() {
    let mut cells = vec![cell("first\nsecond\nthird\nfourth")];
    let original = EntryKey::cell(&cells[0]);
    let mut view = TranscriptView::default();
    view.history = TranscriptHistoryState::Partial;
    index_rows(&mut view, &cells, /*width*/ 20, /*height*/ 3);

    assert!(!view.seek_scrollbar(&cells, /*offset*/ 0, /*at_top*/ false));
    assert!(!view.needs_history(&cells));
    assert!(view.seek_scrollbar(&cells, /*offset*/ 0, /*at_top*/ true));
    assert_eq!(view.history, TranscriptHistoryState::Partial);
    cells.insert(0, cell("old"));
    view.history_page_loaded(&cells, 0..1, /*inserted_images*/ None);

    let metrics = index_rows(&mut view, &cells, /*width*/ 20, /*height*/ 3);
    assert!(metrics.offset > 0);
    assert_eq!(view.visible.first().map(|row| row.key), Some(original));
    assert!(view.near_start(&cells));
    assert!(!view.needs_history(&cells));

    view.scroll(&cells, /*rows*/ -1);
    assert!(view.needs_history(&cells));
}

#[test]
fn scrollbar_heights_follow_width_and_live_output() {
    let cells = [cell("The history row wraps as the viewport narrows.")];
    let mut view = TranscriptView::default();
    let wide = index_rows(&mut view, &cells, /*width*/ 50, /*height*/ 2);
    let narrow = index_rows(&mut view, &cells, /*width*/ 12, /*height*/ 2);
    assert_eq!(wide.total_rows, 1);
    assert!(narrow.total_rows > wide.total_rows);
    assert_eq!(narrow.offset, narrow.total_rows - narrow.viewport_rows);

    view.sync_live_tail(/*width*/ 12, /*key*/ None, |_| {
        Some(vec!["live one".into(), "live two".into()])
    });
    let with_live = index_rows(&mut view, &cells, /*width*/ 12, /*height*/ 2);
    assert_eq!(with_live.total_rows, narrow.total_rows + 3);
    assert_eq!(with_live.offset, narrow.offset + 3);
}

#[test]
fn scrollbar_index_skips_empty_entries() {
    let mut index = RowIndex::default();
    index.resize(4);
    for i in 0..4 {
        index.set(i, Some(EntryHeight::default()));
    }
    index.set(
        0,
        Some(EntryHeight {
            rows: 2,
            separated: false,
        }),
    );
    index.set(
        2,
        Some(EntryHeight {
            rows: 8,
            separated: false,
        }),
    );

    assert_eq!(index.next_unknown(), None);
    index.rebuild_ends();
    assert_eq!(index.rows_before(4), 10);
    assert_eq!(index.row_at(1), Some((0, 1)));
    assert_eq!(index.row_at(2), Some((2, 0)));
    assert_eq!(index.row_at(9), Some((2, 7)));
    assert_eq!(index.row_at(10), None);

    index.invalidate(0..1);
    assert_eq!(index.next_unknown(), Some(0));
    index.set(
        0,
        Some(EntryHeight {
            rows: 3,
            separated: false,
        }),
    );
    assert_eq!(index.next_unknown(), None);
    index.rebuild_ends();
    assert_eq!(index.row_at(3), Some((2, 0)));
}

#[derive(Debug)]
struct CountedCell(Arc<std::sync::atomic::AtomicUsize>);

impl HistoryCell for CountedCell {
    fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        vec!["retained line".into()]
    }

    fn raw_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(/*width*/ 80)
    }
}

#[test]
fn scrollbar_indexing_yields_between_batches_and_reuses_heights() {
    let renders = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut cells = (0..ENTRIES_PER_FRAME * 2)
        .map(|_| Arc::new(CountedCell(Arc::clone(&renders))) as Arc<dyn HistoryCell>)
        .collect::<Vec<_>>();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, /*width*/ 40, /*height*/ 2);
    let before_index = renders.load(std::sync::atomic::Ordering::Relaxed);
    assert!(view.update_scrollbar_navigation(&cells));
    assert!(renders.load(std::sync::atomic::Ordering::Relaxed) <= before_index + ENTRIES_PER_FRAME);

    let measured = index_rows(&mut view, &cells, /*width*/ 40, /*height*/ 2);
    assert_eq!(measured.total_rows, cells.len() * 2 - 1);
    let first_renders = renders.load(std::sync::atomic::Ordering::Relaxed);
    assert!(!view.update_scrollbar_navigation(&cells));
    assert_eq!(
        renders.load(std::sync::atomic::Ordering::Relaxed),
        first_renders
    );

    let start = cells.len() - 1;
    view.invalidate_scrollbar_from(start);
    cells[start] = cell("replacement\nsecond row");
    let after_replacement = index_rows(&mut view, &cells, /*width*/ 40, /*height*/ 2);
    assert_eq!(after_replacement.total_rows, measured.total_rows + 1);
    assert_eq!(
        renders.load(std::sync::atomic::Ordering::Relaxed),
        first_renders
    );
}
