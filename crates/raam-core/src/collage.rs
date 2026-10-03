//! Frameo's collages, landscape screen only:
//!
//! - Layouts are named by photo count and variant (`3_1`, `3_2`, ...).
//!   Each slot is Landscape-only, Portrait-only or Any, and a layout's pick
//!   weight is its constrained-slot count + 1.
//! - The pick walks the ordered queue: take the next
//!   `n = min(collage max, largest layout, left in queue)` items, count their
//!   (landscape, portrait) orientations, keep the size-`n` layouts that accept
//!   that combination, pick one weighted-randomly; with none, `n - 1` and
//!   retry; at `n <= 1`, a single full-screen photo.
//! - Which item goes in which slot: each layout precomputes, per combination,
//!   an orientation for every slot (a DFS over the slots' allowed
//!   orientations, Landscape before Portrait, where a later match replaces
//!   an earlier one). Those (slot, orientation) pairs and the group's items
//!   are each stably sorted by orientation, landscape first, then zipped.
//! - The collage max defaults from the screen diagonal: under 9" 2, under
//!   15" 3, else 4, then clamped to 2..6 by the "Slideshow collage max"
//!   setting.
//!
//! Left out: the 5- and 6-photo layouts (the setting stops at 4 here), the
//! portrait-screen tables, and the one-video / one-greeting-per-group caps
//! (a clip is always a slide of its own here, so no group holds one).

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum Orientation {
    Landscape,
    Portrait,
}

impl Orientation {
    /// Frameo's rule: landscape only if strictly wider than tall.
    pub fn of(width: u32, height: u32) -> Self {
        if width > height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum SlotKind {
    Landscape,
    Portrait,
    Any,
}

impl SlotKind {
    fn allowed(self) -> &'static [Orientation] {
        match self {
            SlotKind::Landscape => &[Orientation::Landscape],
            SlotKind::Portrait => &[Orientation::Portrait],
            SlotKind::Any => &[Orientation::Landscape, Orientation::Portrait],
        }
    }

    fn constrained(self) -> u32 {
        match self {
            SlotKind::Any => 0,
            SlotKind::Landscape | SlotKind::Portrait => 1,
        }
    }
}

/// A slot edge: the screen's start/end, or a guideline at a fraction of
/// the screen.
#[derive(Clone, Copy, Debug)]
enum Edge {
    Start,
    End,
    Guide(f32),
}

/// Which sides carry half a separator (2dp), in the order left, top,
/// right, bottom.
type Margins = [bool; 4];

#[derive(Clone, Copy, Debug)]
struct SlotDef {
    kind: SlotKind,
    edges: [Edge; 4],
    margins: Margins,
}

#[derive(Debug)]
enum Arrangement {
    /// A horizontal row of equally weighted slots.
    Row,
    /// Slots placed between the screen's edges and guidelines.
    Constraint,
}

#[derive(Debug)]
pub struct Layout {
    pub name: &'static str,
    arrangement: Arrangement,
    slots: &'static [SlotDef],
}

use Edge::{End as E, Guide as G, Start as S};
use SlotKind::{Any as A, Landscape as L, Portrait as P};

const fn slot(kind: SlotKind, edges: [Edge; 4], margins: Margins) -> SlotDef {
    SlotDef {
        kind,
        edges,
        margins,
    }
}

const ROW: [Edge; 4] = [S, S, E, E];
const NO: bool = false;
const YES: bool = true;

/// Frameo's landscape collage layouts, sizes 2-4, in its order (the order
/// matters to the weighted pick).
pub static LAYOUTS: [Layout; 7] = [
    Layout {
        name: "2",
        arrangement: Arrangement::Row,
        slots: &[
            slot(A, ROW, [NO, NO, YES, NO]),
            slot(A, ROW, [YES, NO, NO, NO]),
        ],
    },
    Layout {
        name: "3_1",
        arrangement: Arrangement::Constraint,
        slots: &[
            slot(A, [S, S, G(0.33), G(0.5)], [NO, NO, YES, YES]),
            slot(A, [G(0.33), S, E, E], [YES, NO, NO, NO]),
            slot(A, [S, G(0.5), G(0.33), E], [NO, YES, YES, NO]),
        ],
    },
    Layout {
        name: "3_2",
        arrangement: Arrangement::Constraint,
        slots: &[
            slot(L, [S, S, G(0.5), G(0.5)], [NO, NO, YES, YES]),
            slot(P, [G(0.5), S, E, E], [YES, NO, NO, NO]),
            slot(L, [S, G(0.5), G(0.5), E], [NO, YES, YES, NO]),
        ],
    },
    Layout {
        name: "3_3",
        arrangement: Arrangement::Row,
        slots: &[
            slot(P, ROW, [NO, NO, YES, NO]),
            slot(P, ROW, [YES, NO, YES, NO]),
            slot(P, ROW, [YES, NO, NO, NO]),
        ],
    },
    Layout {
        name: "4_1",
        arrangement: Arrangement::Constraint,
        slots: &[
            slot(L, [S, S, G(0.5), G(0.37)], [NO, NO, YES, YES]),
            slot(A, [G(0.5), S, E, G(0.62)], [YES, NO, NO, YES]),
            slot(A, [S, G(0.37), G(0.5), E], [NO, YES, YES, NO]),
            slot(L, [G(0.5), G(0.62), E, E], [YES, YES, NO, NO]),
        ],
    },
    Layout {
        name: "4_2",
        arrangement: Arrangement::Constraint,
        slots: &[
            slot(P, [S, S, G(0.33), E], [NO, NO, YES, NO]),
            slot(A, [G(0.33), S, G(0.66), G(0.5)], [YES, NO, YES, YES]),
            slot(P, [G(0.66), S, E, E], [YES, NO, NO, NO]),
            slot(A, [G(0.33), G(0.5), G(0.66), E], [YES, YES, YES, NO]),
        ],
    },
    Layout {
        name: "4_3",
        arrangement: Arrangement::Constraint,
        slots: &[
            slot(A, [S, S, G(0.5), G(0.5)], [NO, NO, YES, YES]),
            slot(A, [G(0.5), S, E, G(0.5)], [YES, NO, NO, YES]),
            slot(A, [S, G(0.5), G(0.5), E], [NO, YES, YES, NO]),
            slot(A, [G(0.5), G(0.5), E, E], [YES, YES, NO, NO]),
        ],
    },
];

pub use raam_model::limits::LARGEST_LAYOUT;

fn layouts_of_size(n: usize) -> impl Iterator<Item = (usize, &'static Layout)> {
    LAYOUTS
        .iter()
        .enumerate()
        .filter(move |(_, l)| l.slots.len() == n)
}

/// Frameo's default collage max, from the screen diagonal in inches (the
/// raw pixel size over `densityDpi`).
pub fn screen_default_max(width_px: i32, height_px: i32, density_dpi: u32) -> usize {
    let diagonal = screen_diagonal_inches(width_px, height_px, density_dpi);
    if diagonal >= 9.0 {
        if diagonal < 15.0 { 3 } else { 4 }
    } else {
        2
    }
}

pub fn screen_diagonal_inches(width_px: i32, height_px: i32, density_dpi: u32) -> f64 {
    ((width_px as f64).powi(2) + (height_px as f64).powi(2)).sqrt() / density_dpi as f64
}

/// A tile's rectangle in screen pixels, top-left origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x as f32
            && px < (self.x + self.w) as f32
            && py >= self.y as f32
            && py < (self.y + self.h) as f32
    }
}

/// A guideline's pixel position: its fraction of the size, rounded half up.
fn guide_px(fraction: f32, size: i32) -> i32 {
    (0.5 + fraction * size as f32) as i32
}

fn edge_px(edge: Edge, size: i32) -> i32 {
    match edge {
        Edge::Start => 0,
        Edge::End => size,
        Edge::Guide(f) => guide_px(f, size),
    }
}

impl Layout {
    /// Every slot's rectangle on a `w`x`h` screen with `margin` px per
    /// separator half. A row shares out the space left after margins by
    /// weight, as Android does: each slot gets the truncated share of what
    /// is left and the last slot the remainder.
    pub fn rects(&self, w: i32, h: i32, margin: i32) -> Vec<Rect> {
        let m = |on: bool| if on { margin } else { 0 };
        match self.arrangement {
            Arrangement::Row => {
                let margins: i32 = self
                    .slots
                    .iter()
                    .map(|s| m(s.margins[0]) + m(s.margins[2]))
                    .sum();
                let mut excess = w - margins;
                let mut weight_left = self.slots.len() as i32;
                let mut x = 0;
                self.slots
                    .iter()
                    .map(|s| {
                        let share = excess / weight_left;
                        excess -= share;
                        weight_left -= 1;
                        x += m(s.margins[0]);
                        let r = Rect {
                            x,
                            y: 0,
                            w: share,
                            h,
                        };
                        x += share + m(s.margins[2]);
                        r
                    })
                    .collect()
            }
            Arrangement::Constraint => self
                .slots
                .iter()
                .map(|s| {
                    let left = edge_px(s.edges[0], w) + m(s.margins[0]);
                    let top = edge_px(s.edges[1], h) + m(s.margins[1]);
                    let right = edge_px(s.edges[2], w) - m(s.margins[2]);
                    let bottom = edge_px(s.edges[3], h) - m(s.margins[3]);
                    Rect {
                        x: left,
                        y: top,
                        w: right - left,
                        h: bottom - top,
                    }
                })
                .collect(),
        }
    }

    fn weight(&self) -> u32 {
        self.slots.iter().map(|s| s.kind.constrained()).sum::<u32>() + 1
    }

    /// The slot orientations this layout uses for a (landscape, portrait)
    /// count, or `None` if it can't take it. Enumerates depth-first in
    /// Frameo's order and, like Frameo, keeps the LAST match.
    fn slot_orientations(&self, landscape: usize, portrait: usize) -> Option<Vec<Orientation>> {
        fn dfs(
            slots: &[SlotDef],
            i: usize,
            current: &mut Vec<Orientation>,
            want: (usize, usize),
            found: &mut Option<Vec<Orientation>>,
        ) {
            if i == slots.len() {
                let l = current
                    .iter()
                    .filter(|o| **o == Orientation::Landscape)
                    .count();
                if (l, current.len() - l) == want {
                    *found = Some(current.clone());
                }
                return;
            }
            for &o in slots[i].kind.allowed() {
                current.push(o);
                dfs(slots, i + 1, current, want, found);
                current.pop();
            }
        }
        let mut found = None;
        dfs(
            self.slots,
            0,
            &mut Vec::new(),
            (landscape, portrait),
            &mut found,
        );
        found
    }
}

/// One planned collage: the layout (`None` = a single full-screen photo)
/// and, for each slot in layout order, the index into the group taken from
/// the queue.
#[derive(Clone, Debug)]
pub struct Choice {
    pub layout: Option<usize>,
    /// `slot_to_item[slot]` = index into the queue slice passed to `pick`.
    pub slot_to_item: Vec<usize>,
}

impl Choice {
    pub fn consumed(&self) -> usize {
        self.slot_to_item.len()
    }
}

/// Frameo's layout pick for the group at the front of `queue`.
/// `rand_below(n)` must return a uniform integer in `0..n`.
///
/// # Panics
/// If `queue` is empty (a pick for nothing is a caller bug).
pub fn pick(queue: &[Orientation], max: usize, mut rand_below: impl FnMut(u32) -> u32) -> Choice {
    assert!(!queue.is_empty(), "collage pick on an empty queue");
    let mut n = max.min(LARGEST_LAYOUT).min(queue.len());
    loop {
        if n <= 1 {
            return Choice {
                layout: None,
                slot_to_item: vec![0],
            };
        }
        let group = &queue[..n];
        let l = group
            .iter()
            .filter(|o| **o == Orientation::Landscape)
            .count();
        let p = n - l;
        let candidates: Vec<(usize, &Layout, Vec<Orientation>)> = layouts_of_size(n)
            .filter_map(|(i, layout)| layout.slot_orientations(l, p).map(|o| (i, layout, o)))
            .collect();
        // No layout of this size takes the group: shrink it by one and
        // retry. Every landscape size 2-4 has an all-Any layout, so on a
        // landscape screen this never actually happens.
        if !candidates.is_empty() {
            let total: u32 = candidates
                .iter()
                .map(|(_, layout, _)| layout.weight())
                .sum();
            let r = rand_below(total);
            let mut cumulative = 0;
            let mut chosen = candidates.len() - 1;
            // `>=`, not `>`, as Frameo has it (CollageLayoutPickerStrategy:
            // `do { sum += weight } while (sum < r)`): the first candidate
            // wins one draw more than its weight. Kept for fidelity.
            for (k, (_, layout, _)) in candidates.iter().enumerate() {
                cumulative += layout.weight();
                if cumulative >= r {
                    chosen = k;
                    break;
                }
            }
            let (index, _, orientations) = &candidates[chosen];
            return Choice {
                layout: Some(*index),
                slot_to_item: assign(group, orientations),
            };
        }
        n -= 1;
    }
}

/// Zips the (slot, orientation) pairs and the group's items, each stably
/// sorted landscape-first.
fn assign(group: &[Orientation], slot_orientations: &[Orientation]) -> Vec<usize> {
    let mut slots: Vec<usize> = (0..slot_orientations.len()).collect();
    slots.sort_by_key(|&s| slot_orientations[s]);
    let mut items: Vec<usize> = (0..group.len()).collect();
    items.sort_by_key(|&i| group[i]);
    let mut slot_to_item = vec![0; slots.len()];
    for (slot, item) in slots.into_iter().zip(items) {
        slot_to_item[slot] = item;
    }
    slot_to_item
}

#[cfg(test)]
mod tests {
    use super::*;
    use Orientation::{Landscape as Ls, Portrait as Pt};

    #[test]
    fn screen_default_max_by_diagonal() {
        assert_eq!(screen_default_max(1280, 800, 160), 3);
        assert_eq!(screen_default_max(1024, 600, 160), 2);
        assert_eq!(screen_default_max(1920, 1080, 120), 4);
    }

    #[test]
    fn largest_layout_matches_the_table() {
        assert_eq!(
            LAYOUTS.iter().map(|l| l.slots.len()).max(),
            Some(LARGEST_LAYOUT)
        );
    }

    #[test]
    fn rects_1280x800() {
        let two = LAYOUTS[0].rects(1280, 800, 2);
        assert_eq!(
            two,
            vec![
                Rect {
                    x: 0,
                    y: 0,
                    w: 638,
                    h: 800
                },
                Rect {
                    x: 642,
                    y: 0,
                    w: 638,
                    h: 800
                }
            ]
        );
        let thirds = LAYOUTS[3].rects(1280, 800, 2);
        assert_eq!(
            thirds[0],
            Rect {
                x: 0,
                y: 0,
                w: 424,
                h: 800
            }
        );
        assert_eq!(
            thirds[1],
            Rect {
                x: 428,
                y: 0,
                w: 424,
                h: 800
            }
        );
        assert_eq!(
            thirds[2],
            Rect {
                x: 856,
                y: 0,
                w: 424,
                h: 800
            }
        );
        let r31 = LAYOUTS[1].rects(1280, 800, 2);
        // 0.33 * 1280 = 422.4 -> 422.
        assert_eq!(
            r31[0],
            Rect {
                x: 0,
                y: 0,
                w: 420,
                h: 398
            }
        );
        assert_eq!(
            r31[1],
            Rect {
                x: 424,
                y: 0,
                w: 856,
                h: 800
            }
        );
        assert_eq!(
            r31[2],
            Rect {
                x: 0,
                y: 402,
                w: 420,
                h: 398
            }
        );
    }

    #[test]
    fn slot_orientation_last_match_wins() {
        // Layout 2 (Any, Any) with one of each: DFS order LL, LP, PL, PP, so
        // PL is kept - the landscape photo goes right, the portrait left.
        assert_eq!(LAYOUTS[0].slot_orientations(1, 1), Some(vec![Pt, Ls]));
        let c = pick(&[Pt, Ls], 2, |_| 0);
        assert_eq!(c.layout, Some(0));
        assert_eq!(c.slot_to_item, vec![0, 1]);
        let c = pick(&[Ls, Pt], 2, |_| 0);
        assert_eq!(c.slot_to_item, vec![1, 0]);
    }

    #[test]
    fn three_landscape_two_portrait_one_weights() {
        // (2L, 1P) at n = 3: 3_1 (weight 1) and 3_2 (weight 4); 3_1 wins
        // 2 of 5 draws, one more than its weight, as on Frameo.
        let group = [Ls, Pt, Ls];
        let names: Vec<_> = (0..5)
            .map(|r| {
                LAYOUTS[pick(&group, 3, |n| {
                    assert_eq!(n, 5);
                    r
                })
                .layout
                .unwrap()]
                .name
            })
            .collect();
        assert_eq!(names, vec!["3_1", "3_1", "3_2", "3_2", "3_2"]);
        // 3_2 is L, P, L: the landscapes fill slots 0 and 2 in queue order.
        let c = pick(&group, 3, |_| 4);
        assert_eq!(c.slot_to_item, vec![0, 1, 2]);
    }

    #[test]
    fn three_landscape_only_fits_3_1() {
        let c = pick(&[Ls, Ls, Ls], 3, |n| {
            assert_eq!(n, 1);
            0
        });
        assert_eq!(LAYOUTS[c.layout.unwrap()].name, "3_1");
    }

    #[test]
    #[should_panic(expected = "empty queue")]
    fn a_pick_for_nothing_is_a_bug() {
        pick(&[], 3, |_| 0);
    }

    #[test]
    fn max_one_and_short_queue_fall_back_to_single() {
        assert!(pick(&[Ls, Pt, Pt], 1, |_| 0).layout.is_none());
        assert!(pick(&[Pt], 3, |_| 0).layout.is_none());
    }
}
