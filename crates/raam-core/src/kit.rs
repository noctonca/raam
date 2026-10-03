//! The design system, layer 3: the M3 components egui doesn't have, built
//! from egui's own parts (allocate + `Response` + painter), so hit-testing,
//! focus, disabled state and layout stay egui's. Colours only ever come from
//! `theme::scheme(ui)`.
//!
//! Anything egui already draws acceptably once styled (radio, combo box,
//! separator, scroll area, modal) is used as is, through the Style that
//! `theme::install` sets. Its checkbox, slider and text edit didn't pass on
//! the frame, so the kit has its own (the text field keeps egui's editing).
use crate::icons;
use crate::theme::{self, Scheme, Type, layer, scheme, shape, size, space, state};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align, Color32, CornerRadius, FontId, Frame, Galley, Id, Key, LayerId, Margin, Painter, Rangef,
    Rect, Response, Sense, Shape, Stroke, StrokeKind, TextEdit, Ui, UiBuilder, Vec2, pos2, vec2,
};
use std::ops::RangeInclusive;
use std::sync::Arc;

/// The state layer for a response: pressed beats focused beats hovered.
/// With touch input egui only sees a hover while the finger is down (the
/// host sends `PointerGone` on lift), so nothing stays highlighted after a
/// tap; only keys give focus (UX.md, Keys).
fn state_opacity(resp: &Response) -> f32 {
    if resp.is_pointer_button_down_on() {
        state::PRESSED
    } else if resp.has_focus() {
        state::FOCUS
    } else if resp.hovered() {
        state::HOVER
    } else {
        0.0
    }
}

/// `on` over `base` at the response's state opacity. A transparent base
/// (text buttons, icon buttons, list rows) gets `on` at that alpha.
fn with_state(base: Color32, on: Color32, resp: &Response) -> Color32 {
    let o = state_opacity(resp);
    if o == 0.0 {
        base
    } else if base.a() == 0 {
        at_opacity(on, o)
    } else {
        layer(base, on, o)
    }
}

/// `c` at `opacity` (0..1), as the state layers draw it.
fn at_opacity(c: Color32, opacity: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (opacity * 255.0) as u8)
}

fn icon_font(px: f32, filled: bool) -> FontId {
    FontId::new(
        px,
        if filled {
            theme::ICONS_FILL.clone()
        } else {
            theme::ICONS.clone()
        },
    )
}

// ---------------------------------------------------------------------------
// Focus, for a physical keyboard (UX.md, Keys).
// ---------------------------------------------------------------------------

/// The focus ring's width, and its gap outside the control.
const RING: f32 = 3.0;
const RING_GAP: f32 = 2.0;

fn claim_id() -> Id {
    Id::new("kit.claim_focus")
}

/// Who a pending claim is for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Claim {
    /// The next control drawn that can be a screen's first focus.
    First,
    /// This control, which had the focus before.
    On(Id),
}

/// Hands the focus to the next control drawn that can be a screen's first
/// focus: a screen that opened by key calls this just before drawing the
/// part that should take it, and `drop_claim` after that part.
pub fn claim_focus(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(claim_id(), Claim::First));
}

/// Hands the focus back to the control `id` as it draws. Taken even in the
/// pass after a dialog closes, when egui still keeps the focus from what
/// was under it.
pub fn claim_focus_on(ctx: &egui::Context, id: Id) {
    ctx.data_mut(|d| d.insert_temp(claim_id(), Claim::On(id)));
}

/// Ends a claim; true if nothing took it (the part was all disabled, or
/// drawn unseen to size it, or the control has gone).
pub fn drop_claim(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| {
        let pending = d.get_temp::<Claim>(claim_id()).is_some();
        d.remove::<Claim>(claim_id());
        pending
    })
}

/// The controls of the dialog being drawn, in order, while one is.
fn dialog_order_id() -> Id {
    Id::new("kit.dialog_order")
}

/// The focused control that takes ← and → itself (a slider).
fn own_arrows_id() -> Id {
    Id::new("kit.own_arrows")
}

/// Every control that can have the focus calls this as it draws. It takes
/// a pending claim that's for it (one for its id, or, if `first`, one for
/// a screen's first focus), and in a dialog it joins the dialog's order
/// for the arrows (`step_focus`).
fn focusable(ui: &Ui, resp: &Response, first: bool) {
    if !(resp.enabled() && resp.sense.is_focusable()) {
        return;
    }
    let ctx = ui.ctx();
    if ctx.data(|d| d.get_temp::<Vec<Id>>(dialog_order_id()).is_some()) {
        ctx.data_mut(|d| {
            d.get_temp_mut_or_default::<Vec<Id>>(dialog_order_id())
                .push(resp.id)
        });
        // egui's own arrow search would find the page under the dialog.
        ctx.memory_mut(|m| {
            m.set_focus_lock_filter(
                resp.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    ..Default::default()
                },
            )
        });
    }
    let take = match ctx.data(|d| d.get_temp::<Claim>(claim_id())) {
        Some(Claim::First) => first,
        Some(Claim::On(id)) => id == resp.id,
        None => false,
    };
    if take {
        ctx.data_mut(|d| d.remove::<Claim>(claim_id()));
        resp.request_focus();
    }
}

/// Where a control's focus ring goes (UX.md, Keys): 2 px outside it, with
/// 4 px of air round it that the layout makes (12 between controls in a
/// row, between nav items, round the menu's items). Where it can't, the
/// ring goes just inside.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Room {
    /// Outside, unless something shows within 4 px of it last pass (a
    /// layout the kit doesn't space, like two rows of buttons 8 apart):
    /// then inside, a circle's inside its state layer.
    Measure,
    /// Inside: it shares its outline with the next (a segment: Uniform
    /// Connectedness) or sits in another control (the eye in its field).
    Inside,
    /// Outside, always: no ring fits inside (a slider's handle, a switch,
    /// whose handle comes to 4 px of its track's edge). The slider's handle
    /// makes its own room.
    Outside,
}

/// The air an outside ring keeps round it.
const RING_AIR: f32 = 4.0;

/// What showed last pass and this, for `crowded`: every kit control's
/// visible shape (a filled or outlined container, not a text button's
/// clear one), and every line of text and icon the kit drew, with its
/// layer. A rect pushed per shape and line, and the previous pass's list
/// kept: a few hundred rects, pushed under egui's data lock.
#[derive(Clone, Default)]
struct Shown {
    pass: u64,
    now: Vec<(LayerId, Rect)>,
    last: Vec<(LayerId, Rect)>,
}

impl Shown {
    fn roll(&mut self, pass: u64) {
        if self.pass != pass {
            self.last = std::mem::take(&mut self.now);
            self.pass = pass;
        }
    }
}

fn shown_id() -> Id {
    Id::new("kit.shown")
}

/// Notes that something shows at `rect` on `p`'s layer, for the rings of
/// the next pass.
fn shows(p: &Painter, rect: Rect) {
    if !rect.is_positive() {
        return;
    }
    let ctx = p.ctx();
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| {
        let s = d.get_temp_mut_or_default::<Shown>(shown_id());
        s.roll(pass);
        s.now.push((p.layer_id(), rect));
    });
}

/// Whether a ring reaching out to `outer` round the shape `own` comes
/// within `RING_AIR` of anything else in `shown` on its `layer`. What's
/// inside the shape (its label) or holds it (a card round a button) isn't
/// a neighbour. By rects, so a rounded neighbour on the diagonal counts
/// as a little closer than it is.
fn crowded(shown: &[(LayerId, Rect)], layer: LayerId, own: Rect, outer: Rect) -> bool {
    let zone = outer.expand(RING_AIR - 0.5);
    shown.iter().any(|&(l, r)| {
        l == layer
            && zone.intersects(r)
            && !own.expand(0.5).contains_rect(r)
            && !r.expand(0.5).contains_rect(own)
    })
}

/// Whether the ring round `own`, reaching out to `outer`, goes outside it.
fn ring_outside(ui: &Ui, room: Room, own: Rect, outer: Rect) -> bool {
    match room {
        Room::Outside => true,
        Room::Inside => false,
        Room::Measure => {
            let pass = ui.ctx().cumulative_pass_nr();
            let layer = ui.layer_id();
            !ui.ctx().data_mut(|d| {
                let s = d.get_temp_mut_or_default::<Shown>(shown_id());
                s.roll(pass);
                crowded(&s.last, layer, own, outer)
            })
        }
    }
}

/// The focus ring round `shape` while the control has the focus: 2 px
/// outside it, or just inside it where its `room` says. A control that gains it scrolls into view.
fn focus_ring(ui: &Ui, resp: &Response, shape: Rect, corner: CornerRadius, room: Room) {
    if !ring_due(resp) {
        return;
    }
    let s = scheme(ui);
    if !ring_outside(ui, room, shape, shape.expand(RING_GAP + RING)) {
        ui.painter().rect_stroke(
            shape,
            corner,
            Stroke::new(RING, s.secondary),
            StrokeKind::Inside,
        );
        return;
    }
    let grow = |r: u8| r.saturating_add(RING_GAP as u8);
    let corner = CornerRadius {
        nw: grow(corner.nw),
        ne: grow(corner.ne),
        sw: grow(corner.sw),
        se: grow(corner.se),
    };
    ring_painter(ui).rect_stroke(
        shape.expand(RING_GAP),
        corner,
        Stroke::new(RING, s.secondary),
        StrokeKind::Outside,
    );
}

/// Whether a ring is due (the control has the focus); a control that has
/// just gained it scrolls into view.
fn ring_due(resp: &Response) -> bool {
    if resp.gained_focus() {
        resp.scroll_to_me(None);
    }
    resp.has_focus()
}

/// A painter whose clip lets a ring outside a control reach past the part
/// that draws it (the nav pane's top edge clipped the first item's). Grown
/// by only the ring's own reach.
fn ring_painter(ui: &Ui) -> Painter {
    let mut p = ui.painter().clone();
    p.set_clip_rect(ui.clip_rect().expand(RING_GAP + RING));
    p
}

/// The focus ring round a circle of `radius` (an icon button's state
/// layer), as `focus_ring`: inside, it's inside the circle.
fn focus_ring_circle(ui: &Ui, resp: &Response, centre: egui::Pos2, radius: f32, room: Room) {
    let own = Rect::from_center_size(centre, Vec2::splat(radius * 2.0));
    if !ring_due(resp) {
        return;
    }
    let s = scheme(ui);
    if !ring_outside(ui, room, own, own.expand(RING_GAP + RING)) {
        ui.painter()
            .circle_stroke(centre, radius - RING / 2.0, Stroke::new(RING, s.secondary));
        return;
    }
    ring_painter(ui).circle_stroke(
        centre,
        radius + RING_GAP + RING / 2.0,
        Stroke::new(RING, s.secondary),
    );
}

/// The focus ring just inside `shape`, for a control that spans its pane
/// (a list row, a picker's option), where the pane would clip it outside.
fn focus_ring_inside(ui: &Ui, resp: &Response, shape: Rect, corner: CornerRadius) {
    if !ring_due(resp) {
        return;
    }
    let s = scheme(ui);
    ui.painter().rect_stroke(
        shape,
        corner,
        Stroke::new(RING, s.secondary),
        StrokeKind::Inside,
    );
}

// ---------------------------------------------------------------------------
// Centring text on a row.
// ---------------------------------------------------------------------------

/// Roboto's cap height and descent, as fractions of the font size (1456 and
/// 500 of its 2048 units).
pub(crate) const ROBOTO_CAP: f32 = 1456.0 / 2048.0;
pub(crate) const ROBOTO_DESCENT: f32 = 500.0 / 2048.0;

/// A laid-out row's baseline: where its first glyph sits, or `None` for an
/// empty row.
fn first_baseline(r: &egui::epaint::text::PlacedRow) -> Option<f32> {
    r.glyphs.first().map(|g| r.pos.y + g.pos.y)
}

/// Paints `galley` with its first baseline on `y`, rounded to a whole pixel;
/// `x` is its left, centre or right edge by `align`. Returns where it went.
pub(crate) fn galley_on_baseline(
    p: &Painter,
    x: f32,
    align: Align,
    y: f32,
    galley: Arc<Galley>,
    colour: Color32,
) -> Rect {
    let baseline = galley.rows.first().and_then(first_baseline);
    let left = match align {
        Align::Min => x,
        Align::Center => x - galley.size().x / 2.0,
        Align::Max => x - galley.size().x,
    };
    let rect = Rect::from_min_size(
        pos2(
            left.round(),
            y.round() - baseline.unwrap_or(galley.size().y),
        ),
        galley.size(),
    );
    shows(p, galley.mesh_bounds.translate(rect.min.to_vec2()));
    p.galley(rect.min, galley, colour);
    rect
}

/// One line of text with its baseline on `y` (see `galley_on_baseline`).
pub(crate) fn on_baseline(
    p: &Painter,
    x: f32,
    align: Align,
    y: f32,
    text: impl ToString,
    font: FontId,
    colour: Color32,
) -> Rect {
    let galley = p.layout_no_wrap(text.to_string(), font, colour);
    galley_on_baseline(p, x, align, y, galley, colour)
}

/// Paints `galley` with its first line's caps centred on `cy`. Every line of
/// text in the kit is placed this way, the rule that puts it on its row's
/// icon (UX.md: ±0.5 px). Centring the line box instead (`Align2::*_CENTER`)
/// leaves Roboto's caps a pixel high, since its ascent is much larger than
/// its descent, and a half-pixel box then rounds either way.
pub(crate) fn galley_on(
    p: &Painter,
    x: f32,
    align: Align,
    cy: f32,
    galley: Arc<Galley>,
    colour: Color32,
) -> Rect {
    let size = galley
        .job
        .sections
        .first()
        .map_or(0.0, |s| s.format.font_id.size);
    galley_on_baseline(p, x, align, cy + size * ROBOTO_CAP / 2.0, galley, colour)
}

/// One line of `ty` text with its caps centred on `cy`.
pub(crate) fn text_on(
    p: &Painter,
    x: f32,
    align: Align,
    cy: f32,
    text: impl ToString,
    ty: Type,
    colour: Color32,
) -> Rect {
    galley_on(
        p,
        x,
        align,
        cy,
        p.layout_no_wrap(text.to_string(), ty.font(), colour),
        colour,
    )
}

/// An icon with its em box centred on `cy`. Material Symbols draw each glyph
/// in the em box sitting on the baseline, so its centre is half the size
/// above it; egui's `Align2::*_CENTER` centres the taller line box instead
/// (ascent 1056, descent 96 of 960), which puts a 24 px icon 1 px low.
pub(crate) fn icon_on(
    p: &Painter,
    x: f32,
    align: Align,
    cy: f32,
    ch: char,
    font: FontId,
    colour: Color32,
) -> Rect {
    let px = font.size;
    on_baseline(p, x, align, cy + px / 2.0, ch, font, colour)
}

// ---------------------------------------------------------------------------
// What shows, as against what's allocated (UX.md, vertical rhythm).
// ---------------------------------------------------------------------------
//
// Gaps between sections are measured between what shows, not between
// touch targets: a checkbox's 18 px box sits in a 48 px target, and the 15
// px either side is empty. Every kit component reports the rows it paints
// on with `mark_visual`, and `section_header` places itself by them.

/// Material Symbols draw in a 20 px live area centred in their 24 px em.
const ICON_LIVE: f32 = 20.0 / 24.0;

/// The cap top and baseline of a line of `ty` placed by `text_on` at `cy`.
/// Descenders hang below it, as they do into any gap.
pub(crate) fn caps(cy: f32, ty: Type) -> Rangef {
    let px = ty.spec().1;
    let base = (cy + px * ROBOTO_CAP / 2.0).round();
    Rangef::new(base - (px * ROBOTO_CAP).round(), base)
}

/// The live area of a `px` icon placed by `icon_on` at `cy`.
pub(crate) fn icon_ink(cy: f32, px: f32) -> Rangef {
    let base = (cy + px / 2.0).round();
    let pad = px * (1.0 - ICON_LIVE) / 2.0;
    Rangef::new(base - px + pad, base - pad)
}

fn join(a: Rangef, b: Rangef) -> Rangef {
    Rangef::new(a.min.min(b.min), a.max.max(b.max))
}

/// The last row laid out this pass, allocated and visible.
#[derive(Clone, Copy)]
struct LastRow {
    pass: u64,
    alloc: Rangef,
    visual: Rangef,
}

/// The section header whose first row comes next: where that row starts,
/// and the space above the row's visible top the header assumed.
#[derive(Clone, Copy)]
struct Pending {
    pass: u64,
    top: f32,
    header: Id,
    assumed: f32,
}

/// What the first row under a header measured, for its next pass.
#[derive(Clone, Copy)]
struct FirstRow {
    pass: u64,
    slack: f32,
}

fn last_row_id() -> Id {
    Id::new("kit.last_row")
}

fn pending_id() -> Id {
    Id::new("kit.pending_header")
}

/// Tells the section headers around a component what of its allocation
/// `alloc` shows: `visual` is the rows it paints on (a box, an icon's live
/// area, text from cap top to baseline). A component that paints its whole
/// allocation needn't call it. For layouts the kit doesn't draw, like the
/// gallery's icon grid.
pub fn mark_visual(ui: &Ui, alloc: Rect, visual: Rangef) {
    let pass = ui.ctx().cumulative_pass_nr();
    let discard = ui.data_mut(|d| {
        // Components side by side merge into one row; one that starts below
        // the last row starts a new one.
        let row = LastRow {
            pass,
            alloc: alloc.y_range(),
            visual,
        };
        let last = d.get_temp_mut_or_insert_with(last_row_id(), || row);
        if last.pass != pass || alloc.top() >= last.alloc.max - 0.5 {
            *last = row;
        } else {
            last.alloc = join(last.alloc, row.alloc);
            last.visual = join(last.visual, visual);
        }
        // The first row under a header: the smallest slack of its parts.
        let p = d.get_temp::<Pending>(pending_id())?;
        if p.pass != pass || (alloc.top() - p.top).abs() > 0.5 {
            return None;
        }
        let slack = visual.min - alloc.top();
        let first = d.get_temp_mut_or_insert_with(p.header, || FirstRow { pass, slack });
        if first.pass != pass {
            *first = FirstRow { pass, slack };
        } else {
            first.slack = first.slack.min(slack);
        }
        Some(first.slack != p.assumed)
    });
    // The header above was placed for another slack (the page's first pass,
    // or the row changed): lay the page out again before it's shown.
    if discard == Some(true) {
        ui.ctx().request_discard("kit: section header moved");
    }
}

/// One line of `ty` text, cut with an ellipsis at `max_w` rather than
/// wrapped, so it can't break its row's height.
fn one_line(ui: &Ui, text: &str, ty: Type, colour: Color32, max_w: f32) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), ty.font(), colour);
    job.wrap = TextWrapping::truncate_at_width(max_w.max(1.0));
    ui.painter().layout_job(job)
}

/// A disabled Ui already paints at `disabled_alpha` (38%, M3's disabled
/// content), so a colour meant to end up at `alpha` is drawn at this.
fn disabled(c: Color32, alpha: f32) -> Color32 {
    let a = (alpha / state::DISABLED_CONTENT).min(1.0);
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (a * 255.0).round() as u8)
}

// ---------------------------------------------------------------------------
// Buttons.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Filled,
    Tonal,
    Outlined,
    Text,
}

/// An M3 common button: pill, 48 tall, optional leading icon.
pub fn button(ui: &mut Ui, kind: ButtonKind, icon: Option<char>, label: &str) -> Response {
    let s = scheme(ui);
    let font = Type::LabelLarge.font();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let icon_px = 18.0;
    let (lead, trail) = match (kind, icon.is_some()) {
        (ButtonKind::Text, false) => (space::M, space::M),
        (ButtonKind::Text, true) => (space::M, space::L),
        (_, true) => (space::L, space::XL),
        (_, false) => (space::XL, space::XL),
    };
    let icon_w = if icon.is_some() {
        icon_px + space::S
    } else {
        0.0
    };
    let w = (lead + icon_w + galley.size().x + trail).max(size::TOUCH);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::BUTTON_H), Sense::click());
    focusable(ui, &resp, false);
    mark_visual(ui, rect, rect.y_range());
    if ui.is_rect_visible(rect) {
        let (mut container, mut content, mut outline) = match kind {
            ButtonKind::Filled => (s.primary, s.on_primary, Stroke::NONE),
            ButtonKind::Tonal => (
                s.secondary_container,
                s.on_secondary_container,
                Stroke::NONE,
            ),
            ButtonKind::Outlined => (
                Color32::TRANSPARENT,
                s.primary,
                Stroke::new(1.0, s.outline_variant),
            ),
            ButtonKind::Text => (Color32::TRANSPARENT, s.primary, Stroke::NONE),
        };
        if !ui.is_enabled() {
            // M3: every kind goes on-surface, the container at 12% and the
            // content at 38%, rather than its own colours faded (a faded
            // filled button kept its tint and lost its text).
            if container.a() > 0 {
                container = disabled(s.on_surface, state::DISABLED_CONTAINER);
            }
            if outline.width > 0.0 {
                outline.color = disabled(s.on_surface, state::DISABLED_CONTAINER);
            }
            content = s.on_surface;
        }
        let p = ui.painter();
        if kind != ButtonKind::Text {
            shows(p, rect);
        }
        p.rect(
            rect,
            CornerRadius::same(shape::FULL),
            with_state(container, content, &resp),
            outline,
            StrokeKind::Inside,
        );
        let mut x = rect.left() + lead;
        let cy = rect.center().y;
        if let Some(ch) = icon {
            icon_on(p, x, Align::Min, cy, ch, icon_font(icon_px, false), content);
            x += icon_w;
        }
        galley_on(p, x, Align::Min, cy, galley, content);
        focus_ring(
            ui,
            &resp,
            rect,
            CornerRadius::same(shape::FULL),
            Room::Measure,
        );
    }
    resp
}

/// A 48x48 icon button. Selected shows the filled glyph in primary.
pub fn icon_button(ui: &mut Ui, icon: char, selected: bool) -> Response {
    let s = scheme(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(size::TOUCH, size::TOUCH), Sense::click());
    focusable(ui, &resp, false);
    mark_visual(ui, rect, icon_ink(rect.center().y, size::ICON));
    if ui.is_rect_visible(rect) {
        let content = if selected {
            s.primary
        } else {
            s.on_surface_variant
        };
        let bg = with_state(Color32::TRANSPARENT, content, &resp);
        let p = ui.painter();
        p.circle_filled(rect.center(), size::STATE_LAYER_R, bg);
        icon_on(
            p,
            rect.center().x,
            Align::Center,
            rect.center().y,
            icon,
            icon_font(size::ICON, selected),
            content,
        );
        focus_ring_circle(ui, &resp, rect.center(), size::STATE_LAYER_R, Room::Measure);
    }
    resp
}

// ---------------------------------------------------------------------------
// Switch.
// ---------------------------------------------------------------------------

const SWITCH: egui::Vec2 = vec2(52.0, 32.0);

/// Paints an M3 switch with its track centred in `rect`. `t` is 0 (off) to
/// 1 (on), animated by the caller.
fn paint_switch(ui: &Ui, rect: Rect, t: f32, resp: &Response) {
    let s = scheme(ui);
    let track = Rect::from_center_size(rect.center(), SWITCH);
    let on = t > 0.5;
    let p = ui.painter();
    shows(p, track);
    let fill = if on {
        s.primary
    } else {
        s.surface_container_highest
    };
    let stroke = if on {
        Stroke::NONE
    } else {
        Stroke::new(2.0, s.outline)
    };
    p.rect(
        track,
        CornerRadius::same(shape::FULL),
        fill,
        stroke,
        StrokeKind::Inside,
    );
    // Handle: 16 off, 24 on, 28 while pressed.
    let pressed = resp.is_pointer_button_down_on();
    let r = if pressed { 14.0 } else { 8.0 + 4.0 * t };
    let x = egui::lerp((track.left() + 16.0)..=(track.right() - 16.0), t);
    let c = pos2(x, track.center().y);
    let o = state_opacity(resp);
    if o > 0.0 {
        let halo = if on { s.primary } else { s.on_surface };
        p.circle_filled(c, size::STATE_LAYER_R, at_opacity(halo, o));
    }
    p.circle_filled(c, r, if on { s.on_primary } else { s.outline });
    if on {
        icon_on(
            p,
            c.x,
            Align::Center,
            c.y,
            icons::CHECK,
            icon_font(16.0, false),
            s.on_primary_container,
        );
    }
}

pub fn switch(ui: &mut Ui, on: &mut bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(SWITCH.x, size::TOUCH), Sense::click());
    mark_visual(
        ui,
        rect,
        Rangef::point(rect.center().y).expand(SWITCH.y / 2.0),
    );
    focusable(ui, &resp, true);
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, *on, theme::motion::SHORT);
    if ui.is_rect_visible(rect) {
        paint_switch(ui, rect, t, &resp);
        let track = Rect::from_center_size(rect.center(), SWITCH);
        // Inside, the ring would come 1 px from the handle, and the handle's
        // focus layer spills out of the track anyway.
        focus_ring(
            ui,
            &resp,
            track,
            CornerRadius::same(shape::FULL),
            Room::Outside,
        );
    }
    resp
}

// ---------------------------------------------------------------------------
// Checkbox.
// ---------------------------------------------------------------------------

/// An M3 checkbox: an 18 px box in a 48 px target, filled with primary when
/// checked. With a label, the label is part of the target.
///
/// The box, not the target, starts at the cursor, so it sits on the content
/// edge like the fields around it (UX.md, the grid): the target overhangs
/// 15 px to the left, into the padding.
pub fn checkbox(ui: &mut Ui, checked: &mut bool, label: &str) -> Response {
    const BOX: f32 = 18.0;
    let s = scheme(ui);
    let inset = (size::TOUCH - BOX) / 2.0;
    let galley = (!label.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(label.to_owned(), Type::BodyLarge.font(), s.on_surface)
    });
    let w = BOX + galley.as_ref().map_or(inset, |g| space::L + g.size().x);
    let (rect, alloc) = ui.allocate_exact_size(vec2(w, size::TOUCH), Sense::hover());
    mark_visual(ui, rect, Rangef::point(rect.center().y).expand(BOX / 2.0));
    let target = rect.with_min_x(rect.left() - inset);
    let mut resp = ui.interact(target, alloc.id.with("target"), Sense::click());
    focusable(ui, &resp, true);
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    if !ui.is_rect_visible(target) {
        return resp;
    }
    let p = ui.painter();
    let c = pos2(rect.left() + BOX / 2.0, rect.center().y);
    let o = state_opacity(&resp);
    if o > 0.0 {
        let halo = if *checked { s.primary } else { s.on_surface };
        p.circle_filled(c, size::STATE_LAYER_R, at_opacity(halo, o));
    }
    paint_checkbox(p, &s, c, *checked);
    if let Some(g) = galley {
        galley_on(
            p,
            c.x + BOX / 2.0 + space::L,
            Align::Min,
            c.y,
            g,
            s.on_surface,
        );
    }
    focus_ring_circle(ui, &resp, c, size::STATE_LAYER_R, Room::Measure);
    resp
}

/// The checkbox's 18 px box centred on `c`: filled with primary and a check
/// when checked, outlined otherwise.
fn paint_checkbox(p: &Painter, s: &Scheme, c: egui::Pos2, checked: bool) {
    const BOX: f32 = 18.0;
    let b = Rect::from_center_size(c, vec2(BOX, BOX));
    shows(p, b);
    let corner = CornerRadius::same(2);
    if checked {
        p.rect_filled(b, corner, s.primary);
        // The check, as a stroke in the box's own proportions (M3's path).
        let pt = |x: f32, y: f32| b.min + vec2(x, y) * BOX;
        p.line(
            vec![pt(0.22, 0.52), pt(0.42, 0.72), pt(0.80, 0.32)],
            Stroke::new(2.0, s.on_primary),
        );
    } else {
        p.rect_stroke(
            b,
            corner,
            Stroke::new(2.0, s.on_surface_variant),
            StrokeKind::Inside,
        );
    }
}

// ---------------------------------------------------------------------------
// Slider.
// ---------------------------------------------------------------------------

/// An M3 slider (the 2024 shape): a 16 px track, primary up to the value
/// and secondary-container after it, split by a 4 px bar handle with a gap
/// either side. A stop dot marks the end; with a `step`, a dot marks each
/// step instead. While it's held, the value floats above the handle.
///
/// The whole 48 px band is the target: a tap jumps, a drag follows.
///
/// A non-empty `title` goes on a line above the band, as M3 does, with the
/// value (`label`) at its right end; the slider is `slider_width` wide, the
/// same as the fields in its column.
pub fn slider(
    ui: &mut Ui,
    title: &str,
    value: &mut f32,
    range: RangeInclusive<f32>,
    step: Option<f32>,
    label: impl Fn(f32) -> String,
) -> Response {
    const TRACK: f32 = 16.0;
    const HANDLE: egui::Vec2 = vec2(4.0, 44.0);
    const GAP: f32 = 6.0;
    const DOT: f32 = 2.0;
    let s = scheme(ui);
    let (lo, hi) = (*range.start(), *range.end());
    let w = ui.spacing().slider_width.min(ui.available_width());
    // The title line, XS, then the band, as one allocation so the Ui's item
    // spacing can't come between them: title and band together are 72, a
    // two-line row. The XS keeps descenders 6 px off the handle.
    let line = Type::BodyMedium.spec().2;
    let title_h = if title.is_empty() {
        0.0
    } else {
        line + space::XS
    };
    let (whole, alloc) = ui.allocate_exact_size(vec2(w, title_h + size::TOUCH), Sense::hover());
    let rect = whole.with_min_y(whole.top() + title_h);
    let band = Rangef::point(rect.center().y).expand(HANDLE.y / 2.0);
    let title_caps = caps(whole.top() + line / 2.0, Type::BodyMedium);
    mark_visual(
        ui,
        whole,
        if title.is_empty() {
            band
        } else {
            join(title_caps, band)
        },
    );
    let mut resp = ui.interact(rect, alloc.id.with("band"), Sense::click_and_drag());
    focusable(ui, &resp, true);
    // While it has the focus, ← and → change the value rather than move
    // the focus (UX.md, Keys). In a dialog, `focusable` has locked both.
    if resp.has_focus() {
        ui.ctx()
            .data_mut(|d| d.insert_temp(own_arrows_id(), resp.id));
        if ui
            .ctx()
            .data(|d| d.get_temp::<Vec<Id>>(dialog_order_id()).is_none())
        {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                )
            });
        }
    }
    // The handle's centre travels between these: inset by half the track,
    // so the end values sit on the dots in the pills' rounded ends.
    let x0 = rect.left() + TRACK / 2.0;
    let x1 = rect.right() - TRACK / 2.0;
    // A zero step would snap every value to NaN (and a time to 00:00).
    debug_assert!(step.is_none_or(|st| st > 0.0), "slider step {step:?}");
    let snap = |v: f32| {
        let v = step.map_or(v, |st| lo + ((v - lo) / st).round() * st);
        v.clamp(lo, hi)
    };
    let old = *value;
    if let Some(pos) = resp.interact_pointer_pos() {
        let t = ((pos.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
        *value = snap(lo + t * (hi - lo));
    }
    if resp.has_focus() {
        let (r, l) = ui.input(|i| {
            (
                i.num_presses(Key::ArrowRight),
                i.num_presses(Key::ArrowLeft),
            )
        });
        if r + l > 0 {
            // Its own, not a focus step too (the lock holds only from its
            // second pass with the focus).
            ui.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
        }
        let d = r as f32 - l as f32;
        if d != 0.0 {
            *value = snap(*value + d * step.unwrap_or((hi - lo) / 100.0));
        }
    }
    if *value != old {
        resp.mark_changed();
    }
    if !ui.is_rect_visible(whole) {
        return resp;
    }
    let t = if hi > lo {
        ((*value - lo) / (hi - lo)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    // On a whole pixel, so the 4 px handle has crisp edges.
    let hx = egui::lerp(x0..=x1, t).round();
    let cy = rect.center().y;
    let held = resp.is_pointer_button_down_on() || resp.dragged();
    let p = ui.painter();
    if !title.is_empty() {
        let ty = whole.top() + line / 2.0;
        let v = text_on(
            p,
            whole.right(),
            Align::Max,
            ty,
            label(*value),
            Type::BodyMedium,
            s.on_surface_variant,
        );
        let t = one_line(
            ui,
            title,
            Type::BodyMedium,
            s.on_surface,
            v.left() - whole.left() - space::L,
        );
        galley_on(p, whole.left(), Align::Min, ty, t, s.on_surface);
    }
    // With the focus, the gap widens to take the ring and its air, since
    // the handle is too thin for one inside.
    let gap = if resp.has_focus() {
        RING_GAP + RING + RING_AIR
    } else {
        GAP
    };
    shows(
        ui.painter(),
        Rect::from_x_y_ranges(rect.x_range(), Rangef::point(cy).expand(TRACK / 2.0)),
    );
    // Outer ends are pills; the ends at the handle are nearly square.
    let (outer, inner) = ((TRACK / 2.0) as u8, 2u8);
    let active = Rect::from_min_max(
        pos2(rect.left(), cy - TRACK / 2.0),
        pos2(hx - HANDLE.x / 2.0 - gap, cy + TRACK / 2.0),
    );
    let inactive = Rect::from_min_max(
        pos2(hx + HANDLE.x / 2.0 + gap, cy - TRACK / 2.0),
        pos2(rect.right(), cy + TRACK / 2.0),
    );
    if active.width() > 0.0 {
        let r = CornerRadius {
            nw: outer,
            sw: outer,
            ne: inner,
            se: inner,
        };
        p.rect_filled(active, r, s.primary);
    }
    if inactive.width() > 0.0 {
        let r = CornerRadius {
            nw: inner,
            sw: inner,
            ne: outer,
            se: outer,
        };
        p.rect_filled(inactive, r, s.secondary_container);
    }
    // Dots sit where the handle's centre would be at that value, and are
    // hidden under the handle's gap.
    let dot = |x: f32| {
        if (x - hx).abs() > HANDLE.x / 2.0 + gap + DOT {
            let on_active = x < hx;
            p.circle_filled(
                pos2(x, cy),
                DOT,
                if on_active {
                    s.on_primary
                } else {
                    s.on_secondary_container
                },
            );
        }
    };
    match step {
        Some(st) if st > 0.0 && (hi - lo) / st <= 40.0 => {
            let n = ((hi - lo) / st).round() as usize;
            for i in 0..=n {
                dot(egui::lerp(x0..=x1, i as f32 / n as f32));
            }
        }
        _ => {
            if (x1 - hx).abs() > HANDLE.x / 2.0 + gap + DOT {
                p.circle_filled(pos2(x1, cy), DOT, s.primary);
            }
        }
    }
    // The handle narrows while held, as in M3.
    let hw = if held { 2.0 } else { HANDLE.x };
    let handle = Rect::from_center_size(pos2(hx, cy), vec2(hw, HANDLE.y));
    p.rect_filled(handle, CornerRadius::same(2), s.primary);
    focus_ring(ui, &resp, handle, CornerRadius::same(2), Room::Outside);
    if held {
        // Above everything, so neither the card nor a scroll area clips it.
        let fg = ui
            .ctx()
            .layer_painter(egui::LayerId::new(egui::Order::Tooltip, resp.id));
        let g = fg.layout_no_wrap(label(*value), Type::LabelLarge.font(), s.inverse_on_surface);
        let bubble = Rect::from_center_size(
            pos2(hx, rect.top() - space::S - 22.0),
            vec2((g.size().x + space::L * 2.0).max(48.0), 44.0),
        );
        fg.rect_filled(bubble, CornerRadius::same(shape::FULL), s.inverse_surface);
        galley_on(
            &fg,
            bubble.center().x,
            Align::Center,
            bubble.center().y,
            g,
            s.inverse_on_surface,
        );
    }
    resp
}

// ---------------------------------------------------------------------------
// Text field.
// ---------------------------------------------------------------------------

/// An M3 filled text field: 56 px tall, a label that rests inside and moves
/// up once there is focus or text, and an indicator line that thickens to
/// primary on focus. Editing, the cursor and selection are egui's TextEdit,
/// drawn frameless inside it.
pub struct TextField<'a> {
    text: &'a mut String,
    label: &'a str,
    hint: &'a str,
    supporting: Option<&'a str>,
    icon: Option<char>,
    error: bool,
    secret: Option<&'a mut bool>,
    width: Option<f32>,
}

impl<'a> TextField<'a> {
    pub fn new(text: &'a mut String, label: &'a str) -> Self {
        Self {
            text,
            label,
            hint: "",
            supporting: None,
            icon: None,
            error: false,
            secret: None,
            width: None,
        }
    }
    /// Shown in the empty field while it has focus (the label is up then).
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }
    /// One line under the field: help, or the error when `error` is set.
    pub fn supporting(mut self, s: &'a str) -> Self {
        self.supporting = Some(s);
        self
    }
    pub fn icon(mut self, ch: char) -> Self {
        self.icon = Some(ch);
        self
    }
    pub fn error(mut self, error: bool) -> Self {
        self.error = error;
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = Some(w);
        self
    }
    /// A secret (a password, an API key): dots, with a trailing eye that
    /// shows it in the clear; `shown` is whether it does. Every secret
    /// field is this one (UX.md, Similarity). The error icon gives way to
    /// the eye (the colour and the supporting line still say error).
    pub fn secret(mut self, shown: &'a mut bool) -> Self {
        self.secret = Some(shown);
        self
    }

    /// Returns the TextEdit's response (`changed`, `has_focus`, `lost_focus`).
    pub fn show(self, ui: &mut Ui) -> Response {
        const H: f32 = 56.0;
        const SUPPORT_H: f32 = space::XS + 16.0; // BodySmall's line box
        let s = scheme(ui);
        let w = self
            .width
            .unwrap_or(ui.spacing().text_edit_width)
            .min(ui.available_width());
        let total = vec2(
            w,
            H + if self.supporting.is_some() {
                SUPPORT_H
            } else {
                0.0
            },
        );
        let (whole, whole_resp) = ui.allocate_exact_size(total, Sense::hover());
        let field = Rect::from_min_size(whole.min, vec2(w, H));
        // Its 16 px line box starts 4 below the field.
        let support_cy = field.bottom() + space::XS + Type::BodySmall.spec().2 / 2.0;
        let bottom = if self.supporting.is_some() {
            caps(support_cy, Type::BodySmall).max
        } else {
            field.bottom()
        };
        mark_visual(ui, whole, Rangef::new(field.top(), bottom));
        shows(ui.painter(), field);
        // The TextEdit's id derives from this control's own id, so two
        // fields in one Ui can't share ids (see `segmented`).
        let id = whole_resp.id.with("edit");
        let had_focus = ui.memory(|m| m.has_focus(id));

        let left = space::L + self.icon.map_or(0.0, |_| size::ICON + space::M - space::XS);
        let right = if self.error || self.secret.is_some() {
            space::M + size::ICON + space::L
        } else {
            space::L
        };
        let (top, bottom) = if self.label.is_empty() {
            (16.0, 16.0)
        } else {
            (24.0, 8.0)
        };
        // The container goes behind the text, but its colour depends on
        // this frame's focus and hover, which the TextEdit decides.
        let bg = ui.painter().add(Shape::Noop);
        let hint = if had_focus || self.label.is_empty() {
            self.hint
        } else {
            ""
        };
        // `place`, not `put`: `put` moves the cursor back to just under the
        // field, and the next widget in a vertical layout then covered the
        // supporting line (seen on the Server page).
        let resp = ui.place(
            field,
            TextEdit::singleline(self.text)
                .id(id)
                .frame(Frame::new().inner_margin(Margin {
                    left: left as i8,
                    right: right as i8,
                    top: top as i8,
                    bottom: bottom as i8,
                }))
                .font(Type::BodyLarge.font())
                .text_color(s.on_surface)
                .hint_text(theme::text(hint, Type::BodyLarge).color(s.on_surface_variant))
                .password(self.secret.as_deref().is_some_and(|shown| !*shown))
                .desired_width(w)
                .min_size(vec2(w, H))
                .vertical_align(Align::Center),
        );
        // A sub page's first control can be its field (the Join page's).
        focusable(ui, &resp, true);
        // Over the TextEdit, so it takes the tap. The field keeps the
        // focus (and the on-screen keyboard stays) when it had it.
        let eye = self.secret.map(|shown| {
            let r = Rect::from_center_size(
                pos2(
                    field.right() - space::M - size::ICON / 2.0,
                    field.center().y,
                ),
                vec2(size::TOUCH, size::TOUCH),
            );
            let hit = ui.interact(r, id.with("reveal"), Sense::click());
            focusable(ui, &hit, false);
            if hit.clicked() {
                *shown = !*shown;
                if had_focus {
                    ui.memory_mut(|m| m.request_focus(id));
                }
            }
            (*shown, hit)
        });
        let focused = resp.has_focus();
        if !ui.is_rect_visible(whole) {
            return resp;
        }
        let p = ui.painter();
        let top_corners = CornerRadius {
            nw: shape::XS,
            ne: shape::XS,
            sw: 0,
            se: 0,
        };
        let fill = if resp.hovered() && !focused {
            layer(s.surface_container_highest, s.on_surface, state::HOVER)
        } else {
            s.surface_container_highest
        };
        p.set(bg, Shape::rect_filled(field, top_corners, fill));
        let accent = if self.error {
            s.error
        } else if focused {
            s.primary
        } else {
            s.on_surface_variant
        };
        let line_w = if focused { 2.0 } else { 1.0 };
        p.rect_filled(
            Rect::from_min_max(
                pos2(field.left(), field.bottom() - line_w),
                field.right_bottom(),
            ),
            CornerRadius::ZERO,
            accent,
        );
        if let Some(ch) = self.icon {
            icon_on(
                p,
                field.left() + space::M,
                Align::Min,
                field.center().y,
                ch,
                icon_font(size::ICON, false),
                s.on_surface_variant,
            );
        }
        if let Some((shown, hit)) = &eye {
            // An icon button: the eye says what a tap does, show the
            // password or hide it.
            let centre = pos2(
                field.right() - space::M - size::ICON / 2.0,
                field.center().y,
            );
            p.circle_filled(
                centre,
                size::STATE_LAYER_R,
                with_state(Color32::TRANSPARENT, s.on_surface_variant, hit),
            );
            icon_on(
                p,
                centre.x,
                Align::Center,
                centre.y,
                if *shown {
                    icons::VISIBILITY_OFF
                } else {
                    icons::VISIBILITY
                },
                icon_font(size::ICON, false),
                s.on_surface_variant,
            );
            focus_ring_circle(ui, hit, centre, size::STATE_LAYER_R, Room::Inside);
        } else if self.error {
            icon_on(
                p,
                field.right() - space::M,
                Align::Max,
                field.center().y,
                icons::ERROR,
                icon_font(size::ICON, true),
                s.error,
            );
        }
        if !self.label.is_empty() {
            let up = focused || !self.text.is_empty();
            let t = ui
                .ctx()
                .animate_bool_with_time(id.with("label"), up, theme::motion::SHORT);
            // Only the position animates. The size switches halfway, so the
            // atlas holds two sizes of the label, not one per frame.
            let ty = if t < 0.5 {
                Type::BodyLarge
            } else {
                Type::BodySmall
            };
            let y = egui::lerp(field.center().y..=(field.top() + 8.0 + 8.0), t);
            let colour = if self.error {
                s.error
            } else if focused {
                s.primary
            } else {
                s.on_surface_variant
            };
            text_on(
                p,
                field.left() + left,
                Align::Min,
                y,
                self.label,
                ty,
                colour,
            );
        }
        if let Some(sup) = self.supporting {
            let colour = if self.error {
                s.error
            } else {
                s.on_surface_variant
            };
            text_on(
                p,
                field.left() + space::L,
                Align::Min,
                support_cy,
                sup,
                Type::BodySmall,
                colour,
            );
        }
        resp
    }
}

// ---------------------------------------------------------------------------
// Lists (the settings surfaces are made of these).
// ---------------------------------------------------------------------------

pub enum Trailing<'a> {
    None,
    Chevron,
    /// A current value, then a chevron: tapping opens a picker.
    Value(&'a str),
    /// The whole row toggles the switch.
    Switch(&'a mut bool),
    /// A segmented button, `seg_w` per segment. Only the segments are
    /// targets: the row around them isn't, and shows no press state.
    Segmented {
        selected: &'a mut usize,
        options: &'a [&'a str],
        seg_w: f32,
    },
    /// The whole row toggles the checkbox (a multi-select list).
    Checkbox(&'a mut bool),
    /// A text button, for an action on this row's item (Unhide). Only the
    /// button is a target, as with `Segmented`; `changed` when it's tapped.
    /// Its text ends on the trailing edge, its target overhangs.
    Button(&'a str),
    /// `Button` for an action that removes something (Forget). It never
    /// takes a screen's first focus (UX.md, Keys).
    Destructive(&'a str),
}

/// What sits on the content edge of a list item.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Leading {
    /// Nothing: the text starts on the content edge.
    None,
    Icon(char),
    /// An empty icon slot: the text stays on the text edge, in line with the
    /// icon rows around it, for a row no icon would explain.
    Blank,
}

pub struct ListItem<'a> {
    pub leading: Leading,
    pub headline: &'a str,
    pub supporting: Option<&'a str>,
    pub trailing: Trailing<'a>,
    /// The supporting text is a warning (an album "not on the server any
    /// more"), in the warning role rather than on-surface-variant.
    pub warn: bool,
}

impl<'a> ListItem<'a> {
    pub fn new(headline: &'a str) -> Self {
        Self {
            leading: Leading::None,
            headline,
            supporting: None,
            trailing: Trailing::None,
            warn: false,
        }
    }
    /// Shows the supporting text in the warning colour.
    pub fn warning(mut self, warn: bool) -> Self {
        self.warn = warn;
        self
    }
    pub fn icon(mut self, ch: char) -> Self {
        self.leading = Leading::Icon(ch);
        self
    }
    /// Keeps the icon's slot, empty (`Leading::Blank`).
    pub fn blank_icon(mut self) -> Self {
        self.leading = Leading::Blank;
        self
    }
    pub fn supporting(mut self, s: &'a str) -> Self {
        self.supporting = Some(s);
        self
    }
    pub fn trailing(mut self, t: Trailing<'a>) -> Self {
        self.trailing = t;
        self
    }
}

/// An M3 list item: 56 tall, 72 with supporting text, whatever its trailing
/// control (a hand-built row drifts: UX.md). Icon on the content edge, text
/// on the text edge (icon + 16), the trailing control against the right
/// padding. `changed` when its switch or segmented button changed.
pub fn list_item(ui: &mut Ui, mut item: ListItem<'_>) -> Response {
    let s = scheme(ui);
    let h = if item.supporting.is_some() {
        size::LIST_ROW_2LINE
    } else {
        size::LIST_ROW
    };
    let row_target = !matches!(
        item.trailing,
        Trailing::Segmented { .. } | Trailing::Button(_) | Trailing::Destructive(_)
    );
    let sense = if row_target {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, mut resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), sense);
    focusable(ui, &resp, true);
    let switch_t = if let Trailing::Switch(on) = &mut item.trailing {
        if resp.clicked() {
            **on = !**on;
            resp.mark_changed();
        }
        Some(
            ui.ctx()
                .animate_bool_with_time(resp.id, **on, theme::motion::SHORT),
        )
    } else {
        None
    };
    if let Trailing::Checkbox(on) = &mut item.trailing
        && resp.clicked()
    {
        **on = !**on;
        resp.mark_changed();
    }
    let cy = rect.center().y;
    // The press layer fills the whole row, so the row is what shows (UX.md).
    mark_visual(ui, rect, rect.y_range());
    let (hb, sb) = (Type::BodyLarge.spec().2, Type::BodyMedium.spec().2);
    let block_top = cy - (hb + sb) / 2.0;
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let p = ui.painter().clone();
    if row_target {
        p.rect_filled(
            rect,
            CornerRadius::same(shape::M),
            with_state(Color32::TRANSPARENT, s.on_surface, &resp),
        );
    }
    let mut x = rect.left() + space::L;
    if let Leading::Icon(ch) = item.leading {
        icon_on(
            &p,
            x,
            Align::Min,
            cy,
            ch,
            icon_font(size::ICON, false),
            s.on_surface_variant,
        );
    }
    if item.leading != Leading::None {
        x += size::ICON + space::L;
    }
    // Trailing first, so the text knows how much room it has.
    let mut right = rect.right() - space::L;
    let destructive = matches!(item.trailing, Trailing::Destructive(_));
    match &mut item.trailing {
        Trailing::None => {}
        Trailing::Chevron => {
            icon_on(
                &p,
                right,
                Align::Max,
                cy,
                icons::CHEVRON_RIGHT,
                icon_font(size::ICON, false),
                s.on_surface_variant,
            );
            right -= size::ICON + space::S;
        }
        Trailing::Value(v) => {
            icon_on(
                &p,
                right,
                Align::Max,
                cy,
                icons::CHEVRON_RIGHT,
                icon_font(size::ICON, false),
                s.on_surface_variant,
            );
            // XS between the boxes: the chevron's glyph is 8 px inside its
            // box, so value and chevron read 12 (space::M) apart.
            right -= size::ICON + space::XS;
            right = text_on(
                &p,
                right,
                Align::Max,
                cy,
                v,
                Type::BodyMedium,
                s.on_surface_variant,
            )
            .left()
                - space::L;
        }
        Trailing::Switch(_) => {
            let sw = Rect::from_min_max(
                pos2(right - SWITCH.x, rect.top()),
                pos2(right, rect.bottom()),
            );
            paint_switch(ui, sw, switch_t.unwrap_or(0.0), &resp);
            right = sw.left() - space::L;
        }
        Trailing::Segmented {
            selected,
            options,
            seg_w,
        } => {
            let w = *seg_w * options.len() as f32;
            let r = Rect::from_center_size(pos2(right - w / 2.0, cy), vec2(w, size::TOUCH));
            let mut child = ui.new_child(UiBuilder::new().id_salt(resp.id).max_rect(r));
            if segmented(&mut child, selected, options, *seg_w) {
                resp.mark_changed();
            }
            right = r.left() - space::L;
        }
        Trailing::Checkbox(on) => {
            // The box, not a target, ends on the trailing edge: the row is
            // the target.
            let c = pos2(right - 9.0, cy);
            paint_checkbox(&p, &s, c, **on);
            right = c.x - 9.0 - space::L;
        }
        Trailing::Button(label) | Trailing::Destructive(label) => {
            let content = s.primary;
            let g = p.layout_no_wrap(label.to_string(), Type::LabelLarge.font(), content);
            let r = Rect::from_min_max(
                pos2(right - g.size().x - space::M, cy - size::TOUCH / 2.0),
                pos2(right + space::M, cy + size::TOUCH / 2.0),
            );
            let b = ui.interact(r, resp.id.with("button"), Sense::click());
            focusable(ui, &b, !destructive);
            if b.clicked() {
                resp.mark_changed();
            }
            p.rect_filled(
                r,
                CornerRadius::same(shape::FULL),
                with_state(Color32::TRANSPARENT, content, &b),
            );
            galley_on(&p, r.left() + space::M, Align::Min, cy, g, content);
            focus_ring(ui, &b, r, CornerRadius::same(shape::FULL), Room::Measure);
            right = r.left() + space::M - space::L;
        }
    }
    let wrap = right - x;
    let head = one_line(ui, item.headline, Type::BodyLarge, s.on_surface, wrap);
    match item.supporting {
        None => {
            galley_on(&p, x, Align::Min, cy, head, s.on_surface);
        }
        Some(t) => {
            // M3's line boxes, headline 24 then supporting 20, as one 44 px
            // block centred on the row; each line centred in its own box.
            galley_on(&p, x, Align::Min, block_top + hb / 2.0, head, s.on_surface);
            let colour = if item.warn {
                s.warning
            } else {
                s.on_surface_variant
            };
            let sup = one_line(ui, t, Type::BodyMedium, colour, wrap);
            galley_on(&p, x, Align::Min, block_top + hb + sb / 2.0, sup, colour);
        }
    }
    if row_target {
        focus_ring_inside(ui, &resp, rect, CornerRadius::same(shape::M));
    }
    resp
}

/// A page's title (headlineMedium) and subtitle, on the container edge, then
/// 16 to whatever follows. A section header straight after it measures from
/// the subtitle's baseline, as from any other group.
pub fn page_title(ui: &mut Ui, title: &str, sub: &str) {
    let s = scheme(ui);
    ui.label(theme::text(title, Type::HeadlineMedium).color(s.on_surface));
    let shown = paragraph(ui, sub, Type::BodyMedium, s.on_surface_variant);
    // The subtitle already left the Ui's item spacing below itself. The 16
    // is the title's own, so what follows starts at the cursor whatever
    // the page then sets its item spacing to.
    ui.add_space(space::L - ui.spacing().item_spacing.y);
    let block = Rect::from_x_y_ranges(
        ui.max_rect().x_range(),
        Rangef::new(shown.min, ui.cursor().top()),
    );
    mark_visual(ui, block, shown);
}

/// The back arrow's state layer: 34 px, not an icon button's 40, so its
/// ring (22 px from the arrow's centre on 412) keeps 4 px of air to a
/// title on the text edge (440) that starts with a W or a J.
const BACK_R: f32 = 17.0;

/// A sub page's title: `page_title`'s text behind a back arrow, which goes
/// up to the page that opened it (UX.md, Settings). The arrow is a list
/// icon, on the content edge; the title and subtitle move to the text edge
/// after it, so the page keeps the grid's columns. Its caps centre on the
/// title's first line. The lines are laid out as `page_title`'s, so what
/// follows sits where it would on a top-level page. The arrow never takes
/// the page's first focus (its first control does, and Escape steps back
/// by key). Returns the arrow's response.
pub fn page_title_back(ui: &mut Ui, title: &str, sub: &str) -> Response {
    // From the container edge: the content edge, then icon 24 + gap 16.
    const ICON_AT: f32 = space::L;
    const TEXT_AT: f32 = space::L + size::ICON + space::L;
    let s = scheme(ui);
    // One line of the page's text, `TEXT_AT` in: its rows, and its first
    // and last baselines.
    let line = |ui: &mut Ui, text: &str, ty: Type, colour: Color32| {
        let galley = egui::WidgetText::from(theme::text(text, ty).color(colour)).into_galley(
            ui,
            Some(egui::TextWrapMode::Wrap),
            ui.available_width() - TEXT_AT,
            egui::TextStyle::Body,
        );
        let first = galley.rows.first().and_then(first_baseline);
        let last = galley.rows.last().and_then(first_baseline);
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), galley.size().y), Sense::hover());
        ui.painter()
            .galley(pos2(rect.left() + TEXT_AT, rect.top()), galley, colour);
        (rect, first, last)
    };
    let (head, first, _) = line(ui, title, Type::HeadlineMedium, s.on_surface);
    let (rect, sf, sl) = line(ui, sub, Type::BodyMedium, s.on_surface_variant);
    let shown = match (sf, sl) {
        (Some(f), Some(l)) => Rangef::new(
            rect.top() + f - (Type::BodyMedium.spec().1 * ROBOTO_CAP).round(),
            rect.top() + l,
        ),
        _ => rect.y_range(),
    };
    mark_visual(ui, rect, shown);
    ui.add_space(space::L - ui.spacing().item_spacing.y);
    let block = Rect::from_x_y_ranges(
        ui.max_rect().x_range(),
        Rangef::new(shown.min, ui.cursor().top()),
    );
    mark_visual(ui, block, shown);

    let size = Type::HeadlineMedium.font().size;
    let cy = head.top() + first.unwrap_or(size) - size * ROBOTO_CAP / 2.0;
    let centre = pos2(head.left() + ICON_AT + size::ICON / 2.0, cy);
    let target = Rect::from_center_size(centre, vec2(size::TOUCH, size::TOUCH));
    let resp = ui.interact(target, ui.id().with("kit.page_back"), Sense::click());
    focusable(ui, &resp, false);
    // The circle and its ring reach a few px above the title's line box,
    // where a sub page's scroll area would clip them: the clip grows by that
    // overhang, a fixed amount, so a scrolled page still clips the arrow.
    let mut clip = ui.clip_rect();
    clip.min.y -= (head.top() - (cy - BACK_R - RING_GAP - RING)).max(0.0) + 1.0;
    let mut ui = ui.new_child(UiBuilder::new().id_salt("kit.page_back").max_rect(target));
    ui.set_clip_rect(clip);
    let p = ui.painter();
    p.circle_filled(
        centre,
        BACK_R,
        with_state(Color32::TRANSPARENT, s.on_surface, &resp),
    );
    icon_on(
        p,
        centre.x,
        Align::Center,
        cy,
        icons::ARROW_BACK,
        icon_font(size::ICON, false),
        s.on_surface,
    );
    focus_ring_circle(&ui, &resp, centre, BACK_R, Room::Measure);
    resp
}

/// Wrapped text as an egui label, marked from its first line's cap top to
/// its last line's baseline (`mark_visual`). Returns that range.
pub fn paragraph(ui: &mut Ui, text: &str, ty: Type, colour: Color32) -> Rangef {
    let galley = egui::WidgetText::from(theme::text(text, ty).color(colour)).into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        ui.available_width(),
        egui::TextStyle::Body,
    );
    let first = galley.rows.first().and_then(first_baseline);
    let last = galley.rows.last().and_then(first_baseline);
    let rect = ui.label(galley).rect;
    let shown = match (first, last) {
        (Some(f), Some(l)) => Rangef::new(
            rect.top() + f - (ty.spec().1 * ROBOTO_CAP).round(),
            rect.top() + l,
        ),
        _ => rect.y_range(),
    };
    mark_visual(ui, rect, shown);
    shown
}

/// Visible space above a section header's caps, from the last thing that
/// shows in the group before it (UX.md, vertical rhythm).
const SECTION_ABOVE: f32 = space::XL;
/// Visible space from a section header's baseline to the first thing that
/// shows in its group.
const SECTION_BELOW: f32 = space::L;

/// A section's heading, on every page: titleSmall in primary, on the content
/// edge. Placed by what shows, not by allocations (UX.md, vertical rhythm):
/// its caps start `SECTION_ABOVE` below the previous group's visible bottom,
/// and its group's first visible row starts `SECTION_BELOW` under its
/// baseline, whatever the Ui's item spacing. Touch targets overhang into
/// those gaps; they don't widen them.
///
/// The first row's slack (target above its visual) is only known once it's
/// laid out, so the header uses last pass's, and `mark_visual` asks egui to
/// lay the page out again when that changed (a page's first frame).
pub fn section_header(ui: &mut Ui, text: &str) {
    let s = scheme(ui);
    let gap = ui.spacing().item_spacing.y;
    let pass = ui.ctx().cumulative_pass_nr();
    let header = ui.id().with(("kit.section", text));
    let top = ui.cursor().top();
    // The previous allocation ended `gap` above the cursor (or at it, for
    // the page title). If it's the last row a component marked, what shows
    // ends at that row's visual bottom.
    let prev = top - gap;
    let (last, first) = ui.data(|d| {
        (
            d.get_temp::<LastRow>(last_row_id()),
            d.get_temp::<FirstRow>(header),
        )
    });
    let ends_here =
        |r: &LastRow| r.pass == pass && r.alloc.max > prev - 0.5 && r.alloc.max < top + 0.5;
    let shown = last.filter(ends_here).map_or(prev, |r| r.visual.max);
    let slack = first
        .filter(|f| f.pass + 1 == pass)
        .map_or(0.0, |f| f.slack);
    let cap = (Type::TitleSmall.spec().1 * ROBOTO_CAP).round();
    let cap_top = shown + SECTION_ABOVE;
    let rows_top = cap_top + cap + SECTION_BELOW - slack;
    let line = Rect::from_min_max(
        pos2(ui.max_rect().left(), cap_top),
        pos2(ui.max_rect().right(), cap_top + cap),
    );
    if ui.is_rect_visible(line) {
        text_on(
            ui.painter(),
            line.left() + space::L,
            Align::Min,
            line.center().y,
            text,
            Type::TitleSmall,
            s.primary,
        );
    }
    // May be negative: a first row's target reaches up past the caps.
    ui.add_space(rows_top - top);
    ui.data_mut(|d| {
        d.insert_temp(
            pending_id(),
            Pending {
                pass,
                top: rows_top,
                header,
                assumed: slack,
            },
        )
    });
}

/// An M3 navigation-drawer item: the list pane of list/detail.
pub fn nav_item(ui: &mut Ui, icon: char, label: &str, selected: bool) -> Response {
    let s = scheme(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), size::LIST_ROW), Sense::click());
    focusable(ui, &resp, selected);
    if ui.is_rect_visible(rect) {
        let (bg, content) = if selected {
            (s.secondary_container, s.on_secondary_container)
        } else {
            (Color32::TRANSPARENT, s.on_surface_variant)
        };
        let p = ui.painter();
        if selected {
            shows(p, rect);
        }
        p.rect_filled(
            rect,
            CornerRadius::same(shape::FULL),
            with_state(bg, content, &resp),
        );
        let (x, cy) = (rect.left() + space::L, rect.center().y);
        icon_on(
            p,
            x,
            Align::Min,
            cy,
            icon,
            icon_font(size::ICON, selected),
            content,
        );
        text_on(
            p,
            x + size::ICON + space::M,
            Align::Min,
            cy,
            label,
            Type::LabelLarge,
            content,
        );
        focus_ring(
            ui,
            &resp,
            rect,
            CornerRadius::same(shape::FULL),
            Room::Measure,
        );
    }
    // M3's items abut; 12 under each keeps two fills (a press layer and
    // the selected pill) and a ring off each other (UX.md, Keys).
    ui.add_space(space::M);
    resp
}

// ---------------------------------------------------------------------------
// Choices.
// ---------------------------------------------------------------------------

/// An M3 segmented button (single select). Returns true when it changed.
pub fn segmented(ui: &mut Ui, selected: &mut usize, options: &[&str], seg_w: f32) -> bool {
    let s = scheme(ui);
    let n = options.len();
    // The segments' ids derive from the whole control's own id, so two
    // segmented buttons in one Ui can't share ids (with shared ids, egui
    // routes the first control's taps to the second).
    let (rect, whole) = ui.allocate_exact_size(vec2(seg_w * n as f32, size::TOUCH), Sense::hover());
    mark_visual(ui, rect, rect.y_range());
    shows(ui.painter(), rect);
    let mut changed = false;
    let p = ui.painter().clone();
    let cy = rect.center().y;
    let full = shape::FULL;
    let corner_of = |i: usize| CornerRadius {
        nw: if i == 0 { full } else { 0 },
        sw: if i == 0 { full } else { 0 },
        ne: if i + 1 == n { full } else { 0 },
        se: if i + 1 == n { full } else { 0 },
    };
    // Its ring goes on after the outline and the dividers, over them.
    let mut focused = None;
    for (i, label) in options.iter().enumerate() {
        let r = Rect::from_min_size(
            pos2(rect.left() + seg_w * i as f32, rect.top()),
            vec2(seg_w, size::TOUCH),
        );
        let resp = ui.interact(r, whole.id.with(i), Sense::click());
        focusable(ui, &resp, *selected == i);
        if resp.has_focus() {
            focused = Some((r, corner_of(i), resp.clone()));
        }
        if resp.clicked() && *selected != i {
            *selected = i;
            changed = true;
        }
        let on = *selected == i;
        let corner = corner_of(i);
        let (bg, content) = if on {
            (s.secondary_container, s.on_secondary_container)
        } else {
            (Color32::TRANSPARENT, s.on_surface)
        };
        p.rect_filled(r, corner, with_state(bg, content, &resp));
        let g = p.layout_no_wrap(label.to_string(), Type::LabelLarge.font(), content);
        let icon_w = if on { 18.0 + space::S } else { 0.0 };
        let x0 = (r.center().x - (g.size().x + icon_w) / 2.0).round();
        if on {
            icon_on(
                &p,
                x0,
                Align::Min,
                cy,
                icons::CHECK,
                icon_font(18.0, false),
                content,
            );
        }
        galley_on(&p, x0 + icon_w, Align::Min, cy, g, content);
    }
    // One outline round the whole control and a 1 px divider at each join:
    // with every segment stroking its own edges, the joins were 2 px.
    p.rect_stroke(
        rect,
        CornerRadius::same(shape::FULL),
        Stroke::new(1.0, s.outline),
        StrokeKind::Inside,
    );
    for i in 1..n {
        let x = rect.left() + seg_w * i as f32;
        p.rect_filled(
            Rect::from_min_size(pos2(x, rect.top()), vec2(1.0, rect.height())),
            CornerRadius::ZERO,
            s.outline,
        );
    }
    if let Some((mut r, corner, resp)) = focused {
        // Inside, the ring takes in the divider on its right (the one on
        // its left is already in it), so both its sides at a join are 3.
        if r.right() < rect.right() - 0.5 {
            r.max.x += 1.0;
        }
        focus_ring(ui, &resp, r, corner, Room::Inside);
    }
    changed
}

/// An M3 filter chip: 32 tall, in a 48-tall touch target.
pub fn chip(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let s = scheme(ui);
    let g = ui.painter().layout_no_wrap(
        label.to_owned(),
        Type::LabelLarge.font(),
        Color32::PLACEHOLDER,
    );
    let icon_w = if selected { 18.0 + space::S } else { 0.0 };
    let w = space::L + icon_w + g.size().x + space::L;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::TOUCH), Sense::click());
    focusable(ui, &resp, true);
    let chip = Rect::from_center_size(rect.center(), vec2(w, 32.0));
    mark_visual(ui, rect, chip.y_range());
    if ui.is_rect_visible(rect) {
        let (bg, content, stroke) = if selected {
            (
                s.secondary_container,
                s.on_secondary_container,
                Stroke::NONE,
            )
        } else {
            (
                Color32::TRANSPARENT,
                s.on_surface_variant,
                Stroke::new(1.0, s.outline_variant),
            )
        };
        let p = ui.painter();
        shows(p, chip);
        p.rect(
            chip,
            CornerRadius::same(shape::S),
            with_state(bg, content, &resp),
            stroke,
            StrokeKind::Inside,
        );
        let (mut x, cy) = (chip.left() + space::L, chip.center().y);
        if selected {
            icon_on(
                p,
                x,
                Align::Min,
                cy,
                icons::CHECK,
                icon_font(18.0, false),
                content,
            );
            x += icon_w;
        }
        galley_on(p, x, Align::Min, cy, g, content);
        focus_ring(ui, &resp, chip, CornerRadius::same(shape::S), Room::Measure);
    }
    resp
}

// ---------------------------------------------------------------------------
// Containers and messages.
// ---------------------------------------------------------------------------

/// An M3 filled card: surface-container-highest, so it shows on the detail
/// pane's surface-container-low (a card that doesn't is just padding).
/// Filled text fields share its colour, so they don't go in one.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let s = scheme(ui);
    Frame::new()
        .fill(s.surface_container_highest)
        .corner_radius(CornerRadius::same(shape::M))
        .inner_margin(Margin::same(space::L as i8))
        .show(ui, add)
        .inner
}

#[derive(Clone, Copy)]
pub enum Tone {
    Info,
    Warning,
    Error,
}

/// An inline note in a container colour (a missing album's amber). It
/// hugs its text, up to the content width: the container on the container
/// edge, the icon on the content edge and the text on the text edge, like
/// the row above it. One line is 48 tall (12 around the 24 icon).
pub fn note(ui: &mut Ui, tone: Tone, icon: char, text: &str) {
    let s: Scheme = scheme(ui);
    let (bg, fg) = match tone {
        Tone::Info => (s.secondary_container, s.on_secondary_container),
        Tone::Warning => (s.warning_container, s.on_warning_container),
        Tone::Error => (s.error_container, s.on_error_container),
    };
    let line = Type::BodyMedium.spec().2;
    let lead = space::L + size::ICON + space::L;
    let mut job = LayoutJob::simple(
        text.to_owned(),
        Type::BodyMedium.font(),
        fg,
        ui.available_width() - lead - space::L,
    );
    job.sections[0].format.line_height = Some(line);
    let galley = ui.painter().layout_job(job);
    // The first line is centred on the icon; any more follow at the line
    // height.
    let rows = galley.rows.len().max(1) as f32;
    let first = space::M + size::ICON / 2.0;
    let h = first + (rows - 1.0) * line + size::ICON / 2.0 + space::M;
    let (rect, _) =
        ui.allocate_exact_size(vec2(lead + galley.size().x + space::L, h), Sense::hover());
    mark_visual(ui, rect, rect.y_range());
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(shape::M), bg);
        let cy = rect.top() + first;
        icon_on(
            p,
            rect.left() + space::L,
            Align::Min,
            cy,
            icon,
            icon_font(size::ICON, true),
            fg,
        );
        galley_on(p, rect.left() + lead, Align::Min, cy, galley, fg);
    }
}

/// The top app bar's row: title, then actions on the right.
pub fn top_bar(ui: &mut Ui, title: &str, actions: impl FnOnce(&mut Ui)) {
    let s = scheme(ui);
    // Painted, not a label, so its caps are on the actions' centre line.
    let bar = ui.max_rect();
    text_on(
        ui.painter(),
        bar.left() + space::L,
        Align::Min,
        bar.center().y,
        title,
        Type::TitleLarge,
        s.on_surface,
    );
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(space::S);
        actions(ui);
    });
}

/// A top app bar with a navigation icon (back) before the title. The icon
/// sits on the nav pane's icon column (the pane's 12 inset + a nav item's
/// 16), so the bar lines up with the list under it; the title is 8 past
/// the label column (24 + 12 after the icon), clear of the icon's focus
/// ring. Returns the navigation icon's response.
pub fn top_bar_nav(ui: &mut Ui, nav: char, title: &str, actions: impl FnOnce(&mut Ui)) -> Response {
    const ICON_X: f32 = space::M + space::L;
    let s = scheme(ui);
    let bar = ui.max_rect();
    let cy = bar.center().y;
    let target = Rect::from_center_size(
        pos2(bar.left() + ICON_X + size::ICON / 2.0, cy),
        vec2(size::TOUCH, size::TOUCH),
    );
    let resp = ui.interact(target, ui.id().with("kit.top_bar_nav"), Sense::click());
    focusable(ui, &resp, false);
    let p = ui.painter();
    p.circle_filled(
        target.center(),
        size::STATE_LAYER_R,
        with_state(Color32::TRANSPARENT, s.on_surface, &resp),
    );
    icon_on(
        p,
        target.center().x,
        Align::Center,
        cy,
        nav,
        icon_font(size::ICON, false),
        s.on_surface,
    );
    // The title is 8 past the label column, so it's 4 clear of the
    // arrow's ring (its S has no side bearing).
    text_on(
        p,
        bar.left() + ICON_X + size::ICON + space::M + space::S,
        Align::Min,
        cy,
        title,
        Type::TitleLarge,
        s.on_surface,
    );
    focus_ring_circle(
        ui,
        &resp,
        target.center(),
        size::STATE_LAYER_R,
        Room::Measure,
    );
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(space::S);
        actions(ui);
    });
    resp
}

/// One item of a floating toolbar: an icon over its label.
pub struct ToolItem<'a> {
    pub icon: char,
    pub label: &'a str,
    /// Disabled items show at M3's 38% and take no taps.
    pub enabled: bool,
}

/// An M3 floating toolbar over the slideshow, centred at the bottom: a
/// translucent surface-container (so it reads over any photo, see
/// `TOOLBAR_ALPHA`), a shadow to lift it off the picture, optional status
/// lines, then a row of items, each an icon over its label in a 64 px tall
/// target, 8 apart.
///
/// Every item is as wide as the widest of `widest` and the labels, so a
/// label that changes (Hide becoming "Undo hide (5 s)") doesn't move the
/// items under the finger. Returns the index of the item tapped.
pub fn floating_toolbar(
    ctx: &egui::Context,
    id: &str,
    status: &[&str],
    items: &[ToolItem],
    widest: &str,
) -> Option<usize> {
    const ITEM_H: f32 = 64.0;
    // The padding and the GAP between items leave a focus ring 4 px of
    // air to the bar's edge and the next item.
    const PAD: f32 = space::M;
    const GAP: f32 = space::M;
    let s = theme::scheme_of(ctx);
    let mut hit = None;
    // Measured before the Area, and placed at an explicit position: an
    // anchored Area centres by the size it remembers from the last frame, so
    // when the items change (Hide appearing, first run) it stayed off centre
    // until something else asked for a repaint (seen on the frame).
    let label_font = Type::LabelMedium.font();
    let text_w = |t: &str, f: FontId| {
        ctx.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(t.to_owned(), f, Color32::PLACEHOLDER)
                .size()
                .x
        })
    };
    let label_w = items
        .iter()
        .map(|it| text_w(it.label, label_font.clone()))
        .fold(text_w(widest, label_font.clone()), f32::max);
    let item_w = (label_w + 2.0 * space::L).max(80.0).ceil();
    let n = items.len() as f32;
    let row_w = n * item_w + (n - 1.0).max(0.0) * GAP;
    let line = Type::BodyMedium.spec().2;
    let status_w = status
        .iter()
        .map(|t| text_w(t, Type::BodyMedium.font()))
        .fold(0.0, f32::max);
    let status_h = if status.is_empty() {
        0.0
    } else {
        space::M + line * status.len() as f32 + space::XS
    };
    let size = vec2(
        (row_w + 2.0 * PAD).max(status_w + 2.0 * space::XL).ceil(),
        status_h + ITEM_H + 2.0 * PAD,
    );
    let screen = ctx.content_rect();
    let min = pos2(
        (screen.center().x - size.x / 2.0).round(),
        screen.bottom() - space::XL - size.y,
    );
    egui::Area::new(Id::new(id)).fixed_pos(min).show(ctx, |ui| {
        let p = ui.painter().clone();
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let corner = CornerRadius::same(shape::XL);
        let shadow = egui::epaint::Shadow {
            offset: [0, 4],
            blur: 16,
            spread: 0,
            color: Color32::from_black_alpha(if s.dark { 110 } else { 60 }),
        };
        p.add(shadow.as_shape(rect, corner));
        let bg = s.surface_container;
        p.rect_filled(rect, corner, at_opacity(bg, TOOLBAR_ALPHA));
        for (i, t) in status.iter().enumerate() {
            let cy = rect.top() + space::M + line * (i as f32 + 0.5);
            text_on(
                &p,
                rect.center().x,
                Align::Center,
                cy,
                t,
                Type::BodyMedium,
                s.on_surface_variant,
            );
        }
        let row_top = rect.top() + status_h + PAD;
        let x0 = (rect.center().x - row_w / 2.0).round();
        // Nested corners: the container's radius minus its padding.
        let inner = CornerRadius::same(shape::XL - PAD as u8);
        for (i, it) in items.iter().enumerate() {
            let r = Rect::from_min_size(
                pos2(x0 + i as f32 * (item_w + GAP), row_top),
                vec2(item_w, ITEM_H),
            );
            let sense = if it.enabled {
                Sense::click()
            } else {
                Sense::hover()
            };
            let resp = ui.interact(r, Id::new(id).with(("item", i)), sense);
            focusable(ui, &resp, true);
            if it.enabled && resp.clicked() {
                hit = Some(i);
            }
            let content = if it.enabled {
                s.on_surface
            } else {
                s.on_surface.gamma_multiply(state::DISABLED_CONTENT)
            };
            if it.enabled {
                p.rect_filled(
                    r,
                    inner,
                    with_state(Color32::TRANSPARENT, s.on_surface, &resp),
                );
            }
            // Icon 24, 4, a 16 px label line: a 44 px block centred on
            // the item.
            let top = r.top() + (ITEM_H - 44.0) / 2.0;
            icon_on(
                &p,
                r.center().x,
                Align::Center,
                top + size::ICON / 2.0,
                it.icon,
                icon_font(size::ICON, false),
                content,
            );
            text_on(
                &p,
                r.center().x,
                Align::Center,
                top + size::ICON + space::XS + 8.0,
                it.label,
                Type::LabelMedium,
                content,
            );
            focus_ring(ui, &resp, r, inner, Room::Measure);
        }
    });
    hit
}

/// The floating toolbar's container opacity. Text on it keeps its contrast
/// whatever the photo: at 90%, on-surface-variant over surface-container
/// blended with pure white (dark theme) or pure black (light theme) is
/// still above 4.5:1.
pub const TOOLBAR_ALPHA: f32 = 0.9;

/// An M3 filled tonal icon button: a 40 px secondary-container circle in a
/// 48 px target, for a control that needs to look pressable on its own
/// (a stepper's - and +).
pub fn tonal_icon_button(ui: &mut Ui, icon: char) -> Response {
    let s = scheme(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(size::TOUCH, size::TOUCH), Sense::click());
    focusable(ui, &resp, false);
    mark_visual(
        ui,
        rect,
        Rangef::point(rect.center().y).expand(size::STATE_LAYER_R),
    );
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        // Disabled as M3 has it: on-surface, the container at 12% and the
        // icon at 38% (the disabled Ui's own fade makes the icon's 38%).
        let (bg, fg) = if ui.is_enabled() {
            (
                with_state(s.secondary_container, s.on_secondary_container, &resp),
                s.on_secondary_container,
            )
        } else {
            (
                disabled(s.on_surface, state::DISABLED_CONTAINER),
                s.on_surface,
            )
        };
        p.circle_filled(rect.center(), size::STATE_LAYER_R, bg);
        shows(p, Rect::from_center_size(rect.center(), Vec2::splat(40.0)));
        icon_on(
            p,
            rect.center().x,
            Align::Center,
            rect.center().y,
            icon,
            icon_font(size::ICON, false),
            fg,
        );
        focus_ring_circle(ui, &resp, rect.center(), size::STATE_LAYER_R, Room::Measure);
    }
    resp
}

/// How a dialog ended this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogResult<T> {
    Open,
    Dismissed,
    Done(T),
}

/// A dialog's actions: text buttons, right-aligned, in reading order (the
/// confirming one last, on the right). Returns the index tapped. The first
/// (Cancel) takes a claim, never the action it cancels (UX.md, Keys).
pub fn dialog_actions(ui: &mut Ui, labels: &[&str]) -> Option<usize> {
    let mut hit = None;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        for (i, l) in labels.iter().enumerate().rev() {
            let resp = button(ui, ButtonKind::Text, None, l);
            focusable(ui, &resp, i == 0);
            if resp.clicked() {
                hit = Some(i);
            }
        }
    });
    hit
}

/// One choice in a picker: an M3 radio (20 px ring, 10 px dot when
/// selected) on the dialog's content edge and its label 16 after, in a
/// 56 px row that is all target. The press layer reaches the dialog's
/// edges, as M3 dialog lists do.
fn radio_row(ui: &mut Ui, label: &str, selected: bool) -> Response {
    let s = scheme(ui);
    let (rect, resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), size::LIST_ROW), Sense::click());
    focusable(ui, &resp, selected);
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let layer = rect.expand2(vec2(space::XL, 0.0));
        p.rect_filled(
            layer,
            CornerRadius::ZERO,
            with_state(Color32::TRANSPARENT, s.on_surface, &resp),
        );
        focus_ring_inside(ui, &resp, layer, CornerRadius::ZERO);
        let c = pos2(rect.left() + 10.0, rect.center().y);
        let ring = if selected {
            s.primary
        } else {
            s.on_surface_variant
        };
        p.circle_stroke(c, 9.0, Stroke::new(2.0, ring));
        if selected {
            p.circle_filled(c, 5.0, s.primary);
        }
        text_on(
            p,
            rect.left() + 20.0 + space::L,
            Align::Min,
            rect.center().y,
            label,
            Type::BodyLarge,
            s.on_surface,
        );
    }
    resp
}

/// A single-choice picker (M3 simple dialog): a tap on a choice picks it
/// and closes; Cancel or the scrim leaves the value as it was. The target
/// of a `Trailing::Value` row, for five options or more (UX.md, Hick's Law).
pub fn picker(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    options: &[&str],
    selected: usize,
) -> DialogResult<usize> {
    let mut out = DialogResult::Open;
    let open = dialog(ctx, id, title, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, o) in options.iter().enumerate() {
            if radio_row(ui, o, i == selected).clicked() {
                out = DialogResult::Done(i);
            }
        }
        ui.add_space(space::L);
        if dialog_actions(ui, &["Cancel"]).is_some() {
            out = DialogResult::Dismissed;
        }
        out == DialogResult::Open
    });
    if !open && out == DialogResult::Open {
        DialogResult::Dismissed
    } else {
        out
    }
}

/// A number for a dialog: the value large (displayMedium) between - and +
/// tonal buttons, over a slider across `range` for big moves. `step` gives
/// the value one tap of - (`-1`) or + (`1`) leads to, so a picker can step
/// unevenly or wrap; a button whose step wouldn't change the value is
/// disabled. The slider snaps to `slider_step`. `format` writes the value
/// (the big one and the slider's bubble). Returns true when it changed.
pub fn number_picker(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    slider_step: f32,
    step: impl Fn(f32, i32) -> f32,
    format: impl Fn(f32) -> String,
) -> bool {
    let s = scheme(ui);
    let old = *value;
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 72.0), Sense::hover());
    // The buttons' circles on the dialog's content edges.
    let side =
        |x: f32| Rect::from_center_size(pos2(x, row.center().y), vec2(size::TOUCH, size::TOUCH));
    for (dir, x, icon) in [
        (-1, row.left() + size::STATE_LAYER_R, icons::REMOVE),
        (1, row.right() - size::STATE_LAYER_R, icons::ADD),
    ] {
        let next = step(*value, dir);
        let mut child = ui.new_child(UiBuilder::new().max_rect(side(x)).id_salt(("step", dir)));
        if next == *value {
            child.disable();
        }
        if tonal_icon_button(&mut child, icon).clicked() {
            *value = next;
        }
    }
    ui.add_space(space::S);
    ui.spacing_mut().slider_width = ui.available_width();
    let mut v = *value;
    if slider(ui, "", &mut v, range, Some(slider_step), &format).changed() {
        *value = v;
    }
    text_on(
        ui.painter(),
        row.center().x,
        Align::Center,
        row.center().y,
        format(*value),
        Type::DisplayMedium,
        s.on_surface,
    );
    *value != old
}

/// A time of day in `step`-minute steps (a `number_picker`): - and + step
/// it, wrapping at midnight; the slider covers the day.
pub fn time_picker(ui: &mut Ui, minutes: &mut u32, step: u32) -> bool {
    const DAY: u32 = 24 * 60;
    let mut v = *minutes as f32;
    let wrap = |v: f32, dir: i32| ((v as i32 + dir * step as i32).rem_euclid(DAY as i32)) as f32;
    let changed = number_picker(
        ui,
        &mut v,
        0.0..=(DAY - step) as f32,
        step as f32,
        wrap,
        |v| fmt_hm(v as u32),
    );
    *minutes = v as u32;
    changed
}

/// Minutes after midnight as HH:MM (as `schedule::fmt_hm` writes them).
pub fn fmt_hm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

/// An M3 dialog through egui's own Modal: extra-large corners on
/// surface-container-high, over a scrim. Returns false once dismissed.
pub fn dialog(
    ctx: &egui::Context,
    id: &str,
    title: &str,
    body: impl FnOnce(&mut Ui) -> bool,
) -> bool {
    let mut keep = true;
    let s = theme::scheme_of(ctx);
    ctx.data_mut(|d| {
        d.insert_temp(dialog_order_id(), Vec::<Id>::new());
        d.remove::<Id>(own_arrows_id());
    });
    let resp = egui::Modal::new(egui::Id::new(id))
        .backdrop_color(Color32::from_black_alpha(82))
        .frame(
            Frame::new()
                .fill(s.surface_container_high)
                .corner_radius(CornerRadius::same(shape::XL))
                .inner_margin(Margin::same(space::XL as i8)),
        )
        .show(ctx, |ui| {
            ui.set_width(400.0);
            ui.label(theme::text(title, Type::HeadlineSmall).color(s.on_surface));
            ui.add_space(space::L);
            if !body(ui) {
                keep = false;
            }
        });
    if resp.should_close() {
        keep = false;
    }
    let (order, own_arrows) = ctx.data_mut(|d| {
        let order = d.get_temp::<Vec<Id>>(dialog_order_id()).unwrap_or_default();
        d.remove::<Vec<Id>>(dialog_order_id());
        (order, d.get_temp::<Id>(own_arrows_id()))
    });
    step_focus(ctx, &order, own_arrows);
    keep
}

/// The arrows in a dialog step the focus through its controls in the order
/// they're drawn, stopping at the ends: ↓ and → on, ↑ and ← back. A slider
/// keeps ← and → for its value.
fn step_focus(ctx: &egui::Context, order: &[Id], own_arrows: Option<Id>) {
    let Some(focused) = ctx.memory(|m| m.focused()) else {
        return;
    };
    let Some(i) = order.iter().position(|&id| id == focused) else {
        return;
    };
    let (step, any) = ctx.input(|inp| {
        let n = |k| inp.num_presses(k) as i32;
        let horizontal = if own_arrows == Some(focused) {
            0
        } else {
            n(Key::ArrowRight) - n(Key::ArrowLeft)
        };
        let any = [
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
        ]
        .iter()
        .any(|k| n(*k) > 0);
        (n(Key::ArrowDown) - n(Key::ArrowUp) + horizontal, any)
    });
    if !any {
        return;
    }
    // The arrow lock only holds from a control's second pass with the
    // focus: egui mustn't take a step of its own as well.
    ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
    let j = (i as i32 + step).clamp(0, order.len() as i32 - 1) as usize;
    if j != i {
        ctx.memory_mut(|m| m.request_focus(order[j]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 48 px button at `x`, 100 wide, on the content row.
    fn button_at(x: f32) -> Rect {
        Rect::from_min_size(pos2(x, 200.0), vec2(100.0, size::BUTTON_H))
    }

    fn ring_of(r: Rect) -> Rect {
        r.expand(RING_GAP + RING)
    }

    #[test]
    fn minutes_after_midnight_read_as_hh_mm() {
        let cases = [
            (0, "00:00"),
            (5, "00:05"),
            (60, "01:00"),
            (1380, "23:00"),
            (1439, "23:59"),
        ];
        for (min, want) in cases {
            assert_eq!(fmt_hm(min), want, "{min}");
        }
    }

    #[test]
    fn a_ring_is_crowded_by_a_neighbour_8_away_and_not_12() {
        let layer = LayerId::background();
        let own = button_at(400.0);
        let at_8 = [(layer, button_at(own.right() + space::S))];
        let at_12 = [(layer, button_at(own.right() + space::M))];
        assert!(crowded(&at_8, layer, own, ring_of(own)));
        assert!(!crowded(&at_12, layer, own, ring_of(own)));
        // 9 is exactly the ring's reach and its air.
        let at_9 = [(layer, button_at(own.right() + RING_GAP + RING + RING_AIR))];
        assert!(!crowded(&at_9, layer, own, ring_of(own)));
    }

    #[test]
    fn text_close_under_a_ring_crowds_it() {
        let layer = LayerId::background();
        let own = button_at(400.0);
        let line =
            |gap: f32| Rect::from_min_size(pos2(420.0, own.bottom() + gap), vec2(60.0, 14.0));
        assert!(crowded(&[(layer, line(8.0))], layer, own, ring_of(own)));
        assert!(!crowded(&[(layer, line(9.0))], layer, own, ring_of(own)));
    }

    #[test]
    fn its_own_label_and_what_holds_it_are_no_neighbours() {
        let layer = LayerId::background();
        let own = button_at(400.0);
        // Its label, a card round it, and its own shape from last pass.
        let label = Rect::from_min_size(pos2(424.0, 214.0), vec2(52.0, 20.0));
        let card = own.expand(space::S);
        let shown = [(layer, label), (layer, card), (layer, own)];
        assert!(!crowded(&shown, layer, own, ring_of(own)));
    }

    #[test]
    fn a_neighbour_on_another_layer_is_no_neighbour() {
        // The page under a dialog.
        let own = button_at(400.0);
        let page = LayerId::background();
        let dialog = LayerId::new(egui::Order::Foreground, Id::new("dialog"));
        let shown = [(page, button_at(own.right() + space::S))];
        assert!(!crowded(&shown, dialog, own, ring_of(own)));
    }

    #[test]
    fn controls_in_a_row_are_12_apart() {
        // A ring reaches 5 outside its control; 12 leaves it 4 of air to
        // the next, 8 left it 3.
        let ctx = egui::Context::default();
        theme::install(&ctx, theme::Schemes::baked(), theme::Options::default());
        for t in [egui::Theme::Dark, egui::Theme::Light] {
            let gap = ctx.style_of(t).spacing.item_spacing.x;
            assert_eq!(gap, space::M);
            assert!(gap >= RING_GAP + RING + RING_AIR);
        }
    }
}
