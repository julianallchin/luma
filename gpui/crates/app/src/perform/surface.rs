//! The surface's own state, and the edits Setup makes to the layout.
//!
//! A page is a grid. Every cell covers whole grid places and holds one
//! pattern, the master, tap, or a fader of its own. Perform plays the cells;
//! Setup places them from an empty place's "+", drags them to move, drags
//! their corner to resize, names and adds pages, and says which rows play one
//! at a time. The edits are pure, on the layout, so they are tested without a
//! window; the tab saves the layout after each one that changed it.

use luma_lib::models::perform::{Cell, CellContent, LayoutPage, PerformLayout};

/// Which of the tab's two faces is up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum View {
    /// The show: the decks, and the cells to play.
    #[default]
    Perform,
    /// The same grid to arrange, with the source and the inspector.
    Setup,
}

/// The surface's own state: which view and page, the selected cell, the cell
/// being dragged, and the empty place whose "+" menu is open.
#[derive(Default)]
pub(crate) struct Surface {
    pub(crate) view: View,
    pub(crate) page: usize,
    /// The selected cell, by id: what the inspector shows.
    pub(crate) selected: Option<String>,
    /// Where the dragged cell would land, shown until it drops.
    pub(crate) preview: Option<(String, Rect)>,
    pub(crate) adding: Option<Adding>,
}

/// An empty place being filled from its "+" menu, and the group a pattern
/// made there will light.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Adding {
    pub(crate) column: u16,
    pub(crate) row: u16,
    /// A selection expression: a group's name, or `all`.
    pub(crate) group: String,
}

/// Grid places a cell covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub column: u16,
    pub row: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub(crate) fn of(cell: &Cell) -> Self {
        Self {
            column: cell.column,
            row: cell.row,
            width: cell.width,
            height: cell.height,
        }
    }

    /// One grid place.
    pub(crate) fn at(column: u16, row: u16) -> Self {
        Self {
            column,
            row,
            width: 1,
            height: 1,
        }
    }
}

/// The most columns or rows a page has.
pub(crate) const MOST: u16 = 16;

/// A change to the layout or to the surface.
#[derive(Clone, Debug)]
pub(crate) enum Edit {
    View(View),
    Page(usize),
    AddPage,
    /// Remove the current page. A page still holding patterns stays: its
    /// patterns would have nowhere to be.
    RemovePage,
    RenamePage(String),
    /// Size the current page, never smaller than its cells need.
    ResizePage {
        columns: u16,
        rows: u16,
    },
    Select(Option<String>),
    /// Open the "+" menu on an empty place, or close it.
    Adding(Option<(u16, u16)>),
    /// Choose the group a pattern made from the open menu lights.
    AddingGroup(String),
    /// Put a new cell on the current page and select it.
    Place(Rect, CellContent),
    Remove(String),
    /// Change what a cell holds: a fader cell's label and targets.
    SetContent(String, CellContent),
    /// Show `cell` at `rect` while it is dragged.
    Preview(String, Rect),
    /// Drop the dragged cell where it was previewed.
    Drop,
    /// Flip whether a row of the current page plays one at a time.
    OneAtATime(u16),
}

impl Surface {
    /// The page shown, of `layout`.
    pub(crate) fn page<'a>(&self, layout: &'a PerformLayout) -> Option<&'a LayoutPage> {
        layout
            .pages
            .get(self.page.min(layout.pages.len().saturating_sub(1)))
    }

    /// Apply `edit`. Answers whether the layout changed and should be saved.
    pub(crate) fn apply(&mut self, layout: &mut PerformLayout, edit: Edit) -> Result<bool, String> {
        if layout.pages.is_empty() {
            layout.pages.push(blank_page(1));
        }
        self.page = self.page.min(layout.pages.len() - 1);
        let pages = layout.pages.len();
        match edit {
            Edit::View(view) => {
                self.view = view;
                self.preview = None;
                self.adding = None;
                return Ok(false);
            }
            Edit::Page(index) => {
                self.page = index.min(pages - 1);
                self.selected = None;
                self.adding = None;
                return Ok(false);
            }
            Edit::AddPage => {
                layout.pages.push(blank_page(pages + 1));
                self.page = pages;
                self.selected = None;
                self.adding = None;
                return Ok(true);
            }
            Edit::RemovePage => {
                if pages == 1 {
                    return Err("A set keeps at least one page.".into());
                }
                if layout.pages[self.page]
                    .cells
                    .iter()
                    .any(|cell| cell.pattern_id().is_some())
                {
                    return Err("Move or delete this page's patterns first.".into());
                }
                layout.pages.remove(self.page);
                self.page = self.page.saturating_sub(1);
                self.selected = None;
                self.adding = None;
                return Ok(true);
            }
            _ => {}
        }
        let page = &mut layout.pages[self.page];
        match edit {
            Edit::View(_) | Edit::Page(_) | Edit::AddPage | Edit::RemovePage => Ok(false),
            Edit::RenamePage(name) => {
                let name = name.trim();
                if name.is_empty() || name == page.name {
                    return Ok(false);
                }
                page.name = name.to_owned();
                Ok(true)
            }
            Edit::ResizePage { columns, rows } => {
                let used = |end: fn(&Cell) -> u16| page.cells.iter().map(end).max().unwrap_or(1);
                let columns = columns.clamp(used(|c| c.column + c.width).max(1), MOST);
                let rows = rows.clamp(used(|c| c.row + c.height).max(1), MOST);
                if (columns, rows) == (page.columns, page.rows) {
                    return Ok(false);
                }
                page.columns = columns;
                page.rows = rows;
                page.one_at_a_time.retain(|row| *row < rows);
                if self
                    .adding
                    .as_ref()
                    .is_some_and(|adding| adding.column >= columns || adding.row >= rows)
                {
                    self.adding = None;
                }
                Ok(true)
            }
            Edit::Select(id) => {
                self.selected = id;
                self.adding = None;
                Ok(false)
            }
            Edit::Adding(place) => {
                self.adding = place.map(|(column, row)| Adding {
                    column,
                    row,
                    // The group last chosen carries to the next place.
                    group: self
                        .adding
                        .take()
                        .map_or_else(|| "all".to_owned(), |adding| adding.group),
                });
                self.selected = None;
                Ok(false)
            }
            Edit::AddingGroup(group) => {
                if let Some(adding) = self.adding.as_mut() {
                    adding.group = group;
                }
                Ok(false)
            }
            Edit::Place(rect, content) => {
                let cell = Cell {
                    id: uuid::Uuid::new_v4().to_string(),
                    column: rect.column,
                    row: rect.row,
                    width: rect.width,
                    height: rect.height,
                    content,
                };
                if page.misfit(&cell).is_some() {
                    return Err("That place is taken.".into());
                }
                self.selected = Some(cell.id.clone());
                self.adding = None;
                page.cells.push(cell);
                Ok(true)
            }
            Edit::Remove(id) => {
                let before = page.cells.len();
                page.cells.retain(|cell| cell.id != id);
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = None;
                }
                Ok(page.cells.len() != before)
            }
            Edit::SetContent(id, content) => {
                let Some(cell) = page.cells.iter_mut().find(|cell| cell.id == id) else {
                    return Ok(false);
                };
                if cell.content == content {
                    return Ok(false);
                }
                cell.content = content;
                Ok(true)
            }
            Edit::Preview(id, rect) => {
                self.selected = Some(id.clone());
                self.adding = None;
                self.preview = Some((id, rect));
                Ok(false)
            }
            Edit::Drop => {
                let Some((id, rect)) = self.preview.take() else {
                    return Ok(false);
                };
                let Some(at) = page.cells.iter().position(|cell| cell.id == id) else {
                    return Ok(false);
                };
                let moved = Cell {
                    column: rect.column,
                    row: rect.row,
                    width: rect.width,
                    height: rect.height,
                    ..page.cells[at].clone()
                };
                // It lands only where it fits; anywhere else it goes back.
                if page.misfit(&moved).is_some() || moved == page.cells[at] {
                    return Ok(false);
                }
                page.cells[at] = moved;
                Ok(true)
            }
            Edit::OneAtATime(row) => {
                if row >= page.rows {
                    return Ok(false);
                }
                match page.one_at_a_time.iter().position(|one| *one == row) {
                    Some(at) => {
                        page.one_at_a_time.remove(at);
                    }
                    None => {
                        page.one_at_a_time.push(row);
                        page.one_at_a_time.sort_unstable();
                    }
                }
                Ok(true)
            }
        }
    }
}

/// Where a cell `width` × `height` fits on `page`, the first place row by
/// row.
pub(crate) fn room(page: &LayoutPage, width: u16, height: u16) -> Option<Rect> {
    page.free_spot(width, height).map(|(column, row)| Rect {
        column,
        row,
        width,
        height,
    })
}

/// Whether a cell of `page` covers grid place `column`, `row`.
pub(crate) fn covered(page: &LayoutPage, column: u16, row: u16) -> bool {
    page.cells.iter().any(|cell| {
        (cell.column..cell.column + cell.width).contains(&column)
            && (cell.row..cell.row + cell.height).contains(&row)
    })
}

fn blank_page(number: usize) -> LayoutPage {
    LayoutPage {
        id: uuid::Uuid::new_v4().to_string(),
        name: format!("Page {number}"),
        columns: 8,
        rows: 4,
        one_at_a_time: Vec::new(),
        cells: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luma_lib::models::perform::ControlTarget;

    fn layout() -> PerformLayout {
        PerformLayout {
            pages: vec![blank_page(1)],
        }
    }

    fn pattern(id: &str) -> CellContent {
        CellContent::Pattern {
            pattern_id: id.into(),
        }
    }

    fn place(
        surface: &mut Surface,
        layout: &mut PerformLayout,
        rect: Rect,
        content: CellContent,
    ) -> String {
        assert!(surface.apply(layout, Edit::Place(rect, content)).unwrap());
        surface.selected.clone().expect("a placed cell is selected")
    }

    #[test]
    fn a_cell_lands_only_where_it_fits() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        let tall = Rect {
            height: 3,
            ..Rect::at(0, 0)
        };
        let master = place(&mut surface, &mut layout, tall, CellContent::Master);
        assert!(
            surface
                .apply(&mut layout, Edit::Place(Rect::at(0, 2), CellContent::Tap))
                .is_err(),
            "the master covers that place"
        );
        place(&mut surface, &mut layout, Rect::at(1, 0), CellContent::Tap);

        let over = Rect { column: 1, ..tall };
        surface
            .apply(&mut layout, Edit::Preview(master.clone(), over))
            .unwrap();
        assert!(
            !surface.apply(&mut layout, Edit::Drop).unwrap(),
            "tap is there"
        );
        assert_eq!(layout.pages[0].cells[0].column, 0);

        let clear = Rect { column: 5, ..tall };
        surface
            .apply(&mut layout, Edit::Preview(master.clone(), clear))
            .unwrap();
        assert!(surface.apply(&mut layout, Edit::Drop).unwrap());
        assert_eq!(Rect::of(&layout.pages[0].cells[0]), clear);
    }

    #[test]
    fn a_resized_cell_keeps_clear_of_the_others() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        let pad = place(&mut surface, &mut layout, Rect::at(0, 0), pattern("a"));
        place(&mut surface, &mut layout, Rect::at(0, 2), pattern("b"));

        let grown = Rect {
            height: 2,
            ..Rect::at(0, 0)
        };
        surface
            .apply(&mut layout, Edit::Preview(pad.clone(), grown))
            .unwrap();
        assert!(surface.apply(&mut layout, Edit::Drop).unwrap());
        let too_tall = Rect { height: 3, ..grown };
        surface
            .apply(&mut layout, Edit::Preview(pad, too_tall))
            .unwrap();
        assert!(!surface.apply(&mut layout, Edit::Drop).unwrap());
        assert_eq!(layout.pages[0].cells[0].height, 2);
    }

    #[test]
    fn a_row_plays_one_at_a_time_until_flipped_back() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        surface.apply(&mut layout, Edit::OneAtATime(2)).unwrap();
        surface.apply(&mut layout, Edit::OneAtATime(0)).unwrap();
        assert_eq!(layout.pages[0].one_at_a_time, vec![0, 2]);
        surface.apply(&mut layout, Edit::OneAtATime(2)).unwrap();
        assert_eq!(layout.pages[0].one_at_a_time, vec![0]);
        assert!(
            !surface.apply(&mut layout, Edit::OneAtATime(40)).unwrap(),
            "off the page"
        );
    }

    #[test]
    fn pages_are_added_named_and_removed() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        assert!(surface.apply(&mut layout, Edit::AddPage).unwrap());
        assert_eq!(surface.page, 1, "the new page is shown");
        assert!(surface
            .apply(&mut layout, Edit::RenamePage("  Drops ".into()))
            .unwrap());
        assert_eq!(layout.pages[1].name, "Drops");
        assert!(!surface
            .apply(&mut layout, Edit::RenamePage(" ".into()))
            .unwrap());

        place(&mut surface, &mut layout, Rect::at(0, 0), pattern("a"));
        assert!(
            surface.apply(&mut layout, Edit::RemovePage).is_err(),
            "it holds a pattern"
        );
        let cell = layout.pages[1].cells[0].id.clone();
        surface.apply(&mut layout, Edit::Remove(cell)).unwrap();
        assert!(surface.apply(&mut layout, Edit::RemovePage).unwrap());
        assert_eq!(layout.pages.len(), 1);
        assert!(
            surface.apply(&mut layout, Edit::RemovePage).is_err(),
            "the last page stays"
        );
    }

    #[test]
    fn a_page_never_shrinks_under_its_cells() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        let wide = Rect {
            width: 3,
            ..Rect::at(2, 1)
        };
        place(&mut surface, &mut layout, wide, CellContent::Master);
        surface.apply(&mut layout, Edit::OneAtATime(3)).unwrap();
        surface
            .apply(
                &mut layout,
                Edit::ResizePage {
                    columns: 1,
                    rows: 1,
                },
            )
            .unwrap();
        let page = &layout.pages[0];
        assert_eq!((page.columns, page.rows), (5, 2));
        assert!(page.one_at_a_time.is_empty(), "row 3 is gone");
        surface
            .apply(
                &mut layout,
                Edit::ResizePage {
                    columns: 99,
                    rows: 99,
                },
            )
            .unwrap();
        assert_eq!(
            (layout.pages[0].columns, layout.pages[0].rows),
            (MOST, MOST)
        );
    }

    #[test]
    fn the_add_menu_remembers_its_group() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        surface
            .apply(&mut layout, Edit::Adding(Some((3, 1))))
            .unwrap();
        assert_eq!(surface.adding.as_ref().unwrap().group, "all");
        surface
            .apply(&mut layout, Edit::AddingGroup("movers".into()))
            .unwrap();
        surface
            .apply(&mut layout, Edit::Adding(Some((4, 1))))
            .unwrap();
        let adding = surface.adding.clone().unwrap();
        assert_eq!((adding.column, adding.group.as_str()), (4, "movers"));
        place(&mut surface, &mut layout, Rect::at(4, 1), CellContent::Tap);
        assert!(surface.adding.is_none(), "placing closes the menu");
    }

    #[test]
    fn a_forgotten_pattern_leaves_no_cell_and_no_fader_target() {
        let (mut surface, mut layout) = (Surface::default(), layout());
        place(&mut surface, &mut layout, Rect::at(0, 0), pattern("a"));
        let fader = CellContent::Fader {
            label: "Speed".into(),
            targets: vec![
                ControlTarget::Level {
                    pattern_id: "a".into(),
                },
                ControlTarget::Named {
                    name: "drop".into(),
                },
            ],
        };
        place(&mut surface, &mut layout, Rect::at(1, 0), fader);
        layout.forget("a");
        let cells = &layout.pages[0].cells;
        assert_eq!(cells.len(), 1);
        assert!(matches!(
            &cells[0].content,
            CellContent::Fader { targets, .. } if targets.len() == 1
        ));
        assert!(!covered(&layout.pages[0], 0, 0));
        assert!(covered(&layout.pages[0], 1, 0));
    }
}
