//! How the panes of a tab share its area: a tree of splits, evened out and arranged.

use super::*;

/// Panes laid out in a tab: each split holds two parts, side by side or one below the other.
pub(super) enum Node {
    Leaf(u64),
    Split { below: bool, ratio: f32, a: Box<Node>, b: Box<Node> },
}

impl Node {
    pub(super) fn leaves(&self, out: &mut Vec<u64>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    pub(super) fn split(&mut self, at: u64, new: u64, below: bool) -> bool {
        match self {
            Node::Leaf(id) if *id == at => {
                *self = Node::Split { below, ratio: 0.5, a: Box::new(Node::Leaf(at)), b: Box::new(Node::Leaf(new)) };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => a.split(at, new, below) || b.split(at, new, below),
        }
    }

    /// Puts `node` where pane `at` is; whether it was found.
    pub(super) fn replace(&mut self, at: u64, node: Node) -> bool {
        match self {
            Node::Leaf(id) if *id == at => {
                *self = node;
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                if a.leaves_contain(at) {
                    a.replace(at, node)
                } else {
                    b.replace(at, node)
                }
            }
        }
    }

    fn leaves_contain(&self, id: u64) -> bool {
        match self {
            Node::Leaf(x) => *x == id,
            Node::Split { a, b, .. } => a.leaves_contain(id) || b.leaves_contain(id),
        }
    }

    /// The layout without `id`, or nothing when it was the last pane.
    pub(super) fn remove(self, id: u64) -> Option<Node> {
        match self {
            Node::Leaf(x) => (x != id).then_some(Node::Leaf(x)),
            Node::Split { below, ratio, a, b } => match (a.remove(id), b.remove(id)) {
                (Some(a), Some(b)) => Some(Node::Split { below, ratio, a: Box::new(a), b: Box::new(b) }),
                (Some(n), None) | (None, Some(n)) => Some(n),
                (None, None) => None,
            },
        }
    }

    /// How many panes share this part along `below`'s direction: a split the same way counts its
    /// two sides, anything else is one.
    pub(super) fn weight(&self, below: bool) -> f32 {
        match self {
            Node::Split { below: b, a, b: c, .. } if *b == below => a.weight(below) + c.weight(below),
            _ => 1.0,
        }
    }

    /// The same arrangement with every pane in a row or column the same size: three side by
    /// side are a third each, not a half and two quarters.
    /// Whether every pane in each row and column is already the same size.
    pub(super) fn is_even(&self) -> bool {
        match self {
            Node::Leaf(_) => true,
            Node::Split { below, ratio, a, b } => {
                let (wa, wb) = (a.weight(*below), b.weight(*below));
                (*ratio - wa / (wa + wb)).abs() < 0.005 && a.is_even() && b.is_even()
            }
        }
    }

    pub(super) fn balance(&mut self) {
        if let Node::Split { below, ratio, a, b } = self {
            let (wa, wb) = (a.weight(*below), b.weight(*below));
            *ratio = wa / (wa + wb);
            a.balance();
            b.balance();
        }
    }

    /// After `id` was split off along `below`: the row (or column) it is in shares its room
    /// evenly, and every other size set by hand stays.
    pub(super) fn even_out(&mut self, id: u64, below: bool) {
        // The way down to the pane: each split's direction, and whether it went to the first part.
        fn path(n: &Node, id: u64, out: &mut Vec<(bool, bool)>) -> bool {
            match n {
                Node::Leaf(x) => *x == id,
                Node::Split { below, a, b, .. } => {
                    out.push((*below, true));
                    if path(a, id, out) {
                        return true;
                    }
                    out.last_mut().expect("pushed").1 = false;
                    if path(b, id, out) {
                        return true;
                    }
                    out.pop();
                    false
                }
            }
        }
        let mut steps = Vec::new();
        if !path(self, id, &mut steps) {
            return;
        }
        // The run of same-way splits that ends at the new one starts here.
        let start = steps.iter().rposition(|&(b, _)| b != below).map_or(0, |i| i + 1);
        let mut node = self;
        for &(_, first) in &steps[..start] {
            let Node::Split { a, b, .. } = node else { return };
            node = if first { a } else { b };
        }
        node.even_run(below);
    }

    /// Even shares along `below` for a run of splits that way, leaving the parts inside alone.
    pub(super) fn even_run(&mut self, below: bool) {
        if let Node::Split { below: b, ratio, a, b: c } = self
            && *b == below
        {
            let (wa, wc) = (a.weight(below), c.weight(below));
            *ratio = wa / (wa + wc);
            a.even_run(below);
            c.even_run(below);
        }
    }

    /// Panes one after another, beside each other or one above the other, all the same size.
    pub(super) fn even(ids: &[u64], below: bool) -> Node {
        match ids {
            [] => Node::Leaf(0),
            [id] => Node::Leaf(*id),
            [first, rest @ ..] => Node::Split {
                below,
                ratio: 1.0 / ids.len() as f32,
                a: Box::new(Node::Leaf(*first)),
                b: Box::new(Node::even(rest, below)),
            },
        }
    }

    /// The panes in one of the standard arrangements, `main` first where one is bigger.
    pub(super) fn arranged(ids: &[u64], main: u64, layout: Layout) -> Node {
        let mut ids = ids.to_vec();
        if let Some(at) = ids.iter().position(|&id| id == main)
            && matches!(layout, Layout::MainLeft | Layout::MainTop)
        {
            ids.remove(at);
            ids.insert(0, main);
        }
        match (layout, ids.as_slice()) {
            (_, [] | [_]) => Node::even(&ids, false),
            (Layout::Side, _) => Node::even(&ids, false),
            (Layout::Stacked, _) => Node::even(&ids, true),
            (Layout::MainLeft | Layout::MainTop, [first, rest @ ..]) => {
                let below = layout == Layout::MainTop;
                Node::Split { below, ratio: 0.6, a: Box::new(Node::Leaf(*first)), b: Box::new(Node::even(rest, !below)) }
            }
            (Layout::Grid, _) => {
                let cols = (ids.len() as f32).sqrt().ceil() as usize;
                let rows: Vec<Node> = ids.chunks(cols).map(|r| Node::even(r, false)).collect();
                let mut grid = rows.into_iter().rev().reduce(|below_rows, row| Node::Split {
                    below: true,
                    ratio: 0.5,
                    a: Box::new(row),
                    b: Box::new(below_rows),
                });
                let mut grid = grid.take().unwrap_or(Node::Leaf(0));
                grid.balance();
                grid
            }
            (Layout::Even, _) => Node::even(&ids, false),
        }
    }

    pub(super) fn swap(&mut self, x: u64, y: u64) {
        match self {
            Node::Leaf(id) if *id == x => *id = y,
            Node::Leaf(id) if *id == y => *id = x,
            Node::Leaf(_) => {}
            Node::Split { a, b, .. } => {
                a.swap(x, y);
                b.swap(x, y);
            }
        }
    }

    /// The rectangle of each pane; the line between two parts can be dragged.
    pub(super) fn layout(&mut self, ui: &mut egui::Ui, rect: Rect, path: u64, out: &mut Vec<(u64, Rect)>) {
        match self {
            Node::Leaf(id) => out.push((*id, rect)),
            Node::Split { below, ratio, a, b } => {
                const GAP: f32 = 4.0;
                let (ra, rb, line) = if *below {
                    let y = rect.top() + rect.height() * *ratio;
                    let line = Rect::from_min_max(Pos2::new(rect.left(), y - GAP), Pos2::new(rect.right(), y + GAP));
                    (rect.with_max_y(y - GAP / 2.0), rect.with_min_y(y + GAP / 2.0), line)
                } else {
                    let x = rect.left() + rect.width() * *ratio;
                    let line = Rect::from_min_max(Pos2::new(x - GAP, rect.top()), Pos2::new(x + GAP, rect.bottom()));
                    (rect.with_max_x(x - GAP / 2.0), rect.with_min_x(x + GAP / 2.0), line)
                };
                let r = ui.interact(line, ui.scope_id().with(("pane-split", path)), egui::Sense::drag());
                if r.hovered() || r.dragged() {
                    ui.ctx().set_cursor_icon(if *below {
                        egui::CursorIcon::ResizeVertical
                    } else {
                        egui::CursorIcon::ResizeHorizontal
                    });
                }
                if r.dragged() {
                    let d = if *below { r.drag_delta().y / rect.height() } else { r.drag_delta().x / rect.width() };
                    *ratio = (*ratio + d).clamp(0.05, 0.95);
                }
                a.layout(ui, ra, path * 2 + 1, out);
                b.layout(ui, rb, path * 2 + 2, out);
            }
        }
    }
}
