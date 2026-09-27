#![doc = include_str!("../README.md")]

#[cfg(feature = "clipboard")]
mod clipboard;
pub mod layouts;

use crate::layouts::KeyboardLayout;
use egui::{
    pos2, vec2, Align2, Color32, Context, CornerRadius, Event, FontId, Frame, Id, Modifiers, Order,
    Rect, Sense, TextStyle, Ui, Vec2, Window,
};
#[cfg(feature = "clipboard")]
use egui::{Button, WidgetText};
use std::collections::VecDeque;

enum Key {
    Text(&'static str),
    Backspace,
    Upper,
}

/// Main struct for the virtual keyboard. It stores the state of the keyboard and handles the
/// rendering. Needs to be stored between frames.
#[derive(Default)]
pub struct Keyboard {
    input_widget: Option<Id>,
    events: VecDeque<Event>,
    upper: bool,
    keyboard_layout: KeyboardLayout,

    /// How much keyboard is needed. It's a number so we can implement this as some sort of
    /// hysteresis to avoid flickering.
    needed: u32,

    /// Last rect where the keyboard was rendered.
    last_rect: Option<Rect>,

    /// LOCAL ADDITION (experiment 026): labels for the shift, backspace and
    /// Done keys; empty means upstream's "⏶" and "⏴", and "Done". A font
    /// without the arrows (026's Roboto) draws them as "?".
    shift_label: String,
    backspace_label: String,
    done_label: String,
    /// LOCAL ADDITION (experiment 026): keycap colours; unset, they come
    /// from the egui style.
    colours: Option<KeyColours>,
    /// LOCAL ADDITION (experiment 026): Done was tapped this frame, so the
    /// focus must not be handed back to the field.
    done: bool,
}

/// LOCAL ADDITION (experiment 026): the keycaps' colours and corner.
/// Upstream drew frameless buttons, so nothing showed where a key ended.
#[derive(Clone, Copy, Debug)]
pub struct KeyColours {
    /// The keyboard behind the keys.
    pub surface: Color32,
    /// Letter, digit, punctuation and space keys, and their labels.
    pub key: Color32,
    pub on_key: Color32,
    /// Shift, backspace and Done.
    pub function: Color32,
    pub on_function: Color32,
    /// The keycaps' corner radius.
    pub radius: u8,
}

/// LOCAL ADDITION (experiment 026): the key grid's geometry. Rows are 56
/// tall (upstream: 50); keycaps are inset `KEY_GAP / 2` from their cells,
/// and a cell is its key's target, so a tap in a gap still hits a key.
const ROW_H: f32 = 56.0;
const KEY_GAP: f32 = 6.0;
/// Fingers land high on the frame's panel (the owner's taps on backspace
/// came 15 to 25 px above its glyph), so each row's target is moved this
/// far above its keycaps. The top row's target stops at the keyboard's
/// edge (48 px) and the bottom row's reaches the screen's (64 px).
const HIT_RAISE: f32 = 8.0;
/// Shift and backspace are this many letter keys wide.
const WIDE_KEY: f32 = 1.5;

/// One key of the grid, as drawn.
#[derive(Clone, Copy)]
enum Cap {
    Text(&'static str),
    Upper,
    Backspace,
    Space,
    Done,
}

#[cfg(feature = "clipboard")]
fn button(text: impl Into<WidgetText>) -> Button<'static> {
    Button::new(text).frame(false).min_size(Vec2::new(10., 50.))
}

impl Keyboard {
    /// Inject text events into Egui context. This function needs to be called before any widget is
    /// created, otherwise the key presses will be ignored.
    pub fn pump_events(&mut self, ctx: &Context) {
        ctx.input_mut(|input| input.events.extend(std::mem::take(&mut self.events)));
    }

    /// LOCAL ADDITION (experiment 026): the shift, backspace and Done keys'
    /// labels, e.g. icon-font characters.
    pub fn key_labels(
        mut self,
        shift: impl Into<String>,
        backspace: impl Into<String>,
        done: impl Into<String>,
    ) -> Self {
        self.shift_label = shift.into();
        self.backspace_label = backspace.into();
        self.done_label = done.into();
        self
    }

    /// LOCAL ADDITION (experiment 026): the keycaps' colours; set them
    /// every frame to follow a theme switch.
    pub fn set_colours(&mut self, colours: KeyColours) {
        self.colours = Some(colours);
    }

    pub fn layout(mut self, layout: KeyboardLayout) -> Self {
        self.keyboard_layout = layout;
        self
    }

    /// Area which is free from the keyboard. This is useful when you want to constrain a window to
    /// the area which is not covered by the keyboard.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # egui::__run_test_ctx(|ctx| {
    /// # let keyboard = egui_keyboard::Keyboard::default();
    /// egui::Window::new("Hello")
    ///   .constrain_to(keyboard.safe_rect(ctx))
    ///   .show(ctx, |ui| {
    ///      ui.label("it is a window");
    ///   });
    /// # });
    /// ```
    /// LOCAL ADDITION (experiment 018): the exact rect the keyboard last
    /// rendered at, for real touch-coordinate verification instead of
    /// guessing key positions from a screenshot.
    pub fn last_rect(&self) -> Option<Rect> {
        self.last_rect
    }

    pub fn safe_rect(&self, ctx: &Context) -> Rect {
        let screen_rect = ctx.content_rect();

        if let Some(last_rect) = self.last_rect {
            Rect::from_min_max(
                screen_rect.min,
                screen_rect.max - vec2(0., last_rect.height()),
            )
        } else {
            screen_rect
        }
    }

    /// Shows the virtual keyboard if needed.
    pub fn show(&mut self, ctx: &Context) {
        self.done = false;
        self.remember_input_widget(ctx);

        if self.keyboard_input_needed(ctx) {
            let keys = self.keyboard_layout.get_keys(self.upper);
            let style = ctx.global_style();
            let v = &style.visuals;
            let colours = self.colours.unwrap_or(KeyColours {
                surface: v.extreme_bg_color,
                key: v.widgets.inactive.bg_fill,
                on_key: v.text_color(),
                function: v.widgets.inactive.weak_bg_fill,
                on_function: v.widgets.inactive.fg_stroke.color,
                radius: 8,
            });

            let response = Window::new("Keyboard")
                .frame(Frame::NONE.fill(colours.surface))
                .collapsible(false)
                .resizable(false)
                .title_bar(false)
                .anchor(Align2::CENTER_BOTTOM, [0., 0.])
                .fixed_size(vec2(ctx.content_rect().width(), 0.))
                .order(Order::Foreground)
                .show(ctx, |ui| {
                    // We do not want any spacing between the keys.
                    ui.style_mut().spacing.item_spacing = Vec2::ZERO;

                    #[cfg(feature = "clipboard")]
                    self.clipboard_key(ui);

                    // LOCAL CHANGE (experiment 026): keycaps, drawn by
                    // `grid` instead of a column of frameless buttons per
                    // key.
                    self.grid(ui, &keys, colours);
                });

            if let Some(response) = response {
                self.last_rect = Some(response.response.rect);

                if response.response.contains_pointer() && !std::mem::take(&mut self.done) {
                    // Make sure Egui still thinks that we need the keyboard in the next frame.
                    self.focus_back_to_input_widget(ctx);
                }
            }

            // Prevent native keyboard from showing up.
            ctx.output_mut(|output| {
                output.ime = None;
            });
        } else {
            self.last_rect = None;
        }
    }

    #[cfg(feature = "clipboard")]
    fn clipboard_key(&mut self, ui: &mut Ui) {
        if let Some(text) = clipboard::get_text() {
            if ui.add(button(trim_text(&text, 20))).clicked() {
                let event = Event::Text(text.to_string());
                self.events.push_back(event);
                self.focus_back_to_input_widget(ui.ctx());
            }
        }
    }

    /// Remember which widget had focus before the keyboard was shown.
    fn remember_input_widget(&mut self, ctx: &Context) {
        if ctx.egui_wants_keyboard_input() {
            self.input_widget = ctx.memory(|memory| memory.focused());
        }
    }

    /// Focus back to the previously focused widget.
    fn focus_back_to_input_widget(&mut self, ctx: &Context) {
        if let Some(focus) = self.input_widget {
            ctx.memory_mut(|memory| memory.request_focus(focus));
        }
    }

    /// LOCAL ADDITION (experiment 026): the keys as keycaps. Each row is
    /// split into cells by key width (1, or `WIDE_KEY` for shift and
    /// backspace); the bottom row, a lone space key upstream, is a space bar
    /// over the middle five of ten key widths with Done over the last one.
    /// A cell is its key's target (raised by `HIT_RAISE`); its keycap is the
    /// cell inset by half of `KEY_GAP`.
    fn grid(&mut self, ui: &mut Ui, keys: &[Vec<Key>], c: KeyColours) {
        let n_rows = keys.len();
        let (area, _) = ui.allocate_exact_size(
            vec2(ui.available_width(), ROW_H * n_rows as f32),
            Sense::hover(),
        );
        let font = ui
            .style()
            .text_styles
            .get(&TextStyle::Heading)
            .cloned()
            .unwrap_or(FontId::proportional(22.0));
        let painter = ui.painter().clone();
        let mut tapped = None;
        for (r, row) in keys.iter().enumerate() {
            let top = area.top() + r as f32 * ROW_H;
            let hit_top = if r == 0 { area.top() } else { top - HIT_RAISE };
            let hit_bottom = if r + 1 == n_rows {
                area.bottom()
            } else {
                top + ROW_H - HIT_RAISE
            };
            // (left, right) in key widths, and the key.
            let cells: Vec<(f32, f32, Cap)> = if let [Key::Text(" ")] = row.as_slice() {
                let unit = area.width() / 10.0;
                vec![
                    (2.5 * unit, 7.5 * unit, Cap::Space),
                    (9.0 * unit, 10.0 * unit, Cap::Done),
                ]
            } else {
                let caps: Vec<(f32, Cap)> = row
                    .iter()
                    .map(|k| match k {
                        Key::Text(t) => (1.0, Cap::Text(t)),
                        Key::Upper => (WIDE_KEY, Cap::Upper),
                        Key::Backspace => (WIDE_KEY, Cap::Backspace),
                    })
                    .collect();
                let unit = area.width() / caps.iter().map(|(w, _)| w).sum::<f32>();
                let mut x = 0.0;
                caps.into_iter()
                    .map(|(w, cap)| {
                        x += w * unit;
                        (x - w * unit, x, cap)
                    })
                    .collect()
            };
            for (i, (x0, x1, cap)) in cells.into_iter().enumerate() {
                let (x0, x1) = (area.left() + x0, area.left() + x1);
                let hit = Rect::from_min_max(pos2(x0, hit_top), pos2(x1, hit_bottom));
                let resp = ui.interact(hit, ui.id().with(("key", r, i)), Sense::click());
                let keycap =
                    Rect::from_min_max(pos2(x0, top), pos2(x1, top + ROW_H)).shrink(KEY_GAP / 2.0);
                let function = matches!(cap, Cap::Upper | Cap::Backspace | Cap::Done);
                let (fill, on) = if function {
                    (c.function, c.on_function)
                } else {
                    (c.key, c.on_key)
                };
                // The pressed state: the label's colour over the fill at 10%.
                let fill = if resp.is_pointer_button_down_on() {
                    mix(fill, on, 0.10)
                } else {
                    fill
                };
                painter.rect_filled(keycap, CornerRadius::same(c.radius), fill);
                let label = match cap {
                    Cap::Text(t) => t.to_string(),
                    Cap::Upper => {
                        if self.shift_label.is_empty() {
                            "⏶".into()
                        } else {
                            self.shift_label.clone()
                        }
                    }
                    Cap::Backspace => {
                        if self.backspace_label.is_empty() {
                            "⏴".into()
                        } else {
                            self.backspace_label.clone()
                        }
                    }
                    Cap::Space => String::new(),
                    Cap::Done => {
                        if self.done_label.is_empty() {
                            "Done".into()
                        } else {
                            self.done_label.clone()
                        }
                    }
                };
                painter.text(
                    keycap.center(),
                    Align2::CENTER_CENTER,
                    label,
                    font.clone(),
                    on,
                );
                if resp.clicked() {
                    tapped = Some(cap);
                }
            }
        }
        let backspace = Event::Key {
            key: egui::Key::Backspace,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::NONE,
            physical_key: None,
        };
        match tapped {
            None => return,
            Some(Cap::Text(t)) => self.events.push_back(Event::Text(t.to_string())),
            Some(Cap::Space) => self.events.push_back(Event::Text(" ".into())),
            Some(Cap::Backspace) => self.events.push_back(backspace),
            Some(Cap::Upper) => self.upper = !self.upper,
            Some(Cap::Done) => {
                // Put the keyboard away: the field loses focus, and the
                // hysteresis that keeps the keyboard up through focus blips
                // ends.
                if let Some(id) = self.input_widget.take() {
                    ui.ctx().memory_mut(|m| m.surrender_focus(id));
                }
                self.needed = 0;
                self.done = true;
                return;
            }
        }
        self.focus_back_to_input_widget(ui.ctx());
    }

    /// LOCAL PATCH (experiment 018): upstream called `ctx.request_repaint()`
    /// on every frame the keyboard was shown at all, including the common
    /// case where a field simply has focus and nothing is actually changing
    /// (no touch, no animation of ours - `TextEdit`'s own cursor blink
    /// already schedules its own repaints via `request_repaint_after`).
    /// Against a backend that honours egui's requested repaint delay instead
    /// of redrawing unconditionally every loop iteration, that pinned the
    /// render loop to max rate for as long as the keyboard stayed open,
    /// measured on real hardware dropping ~47fps to ~16fps while idle with
    /// the keyboard up. Only the brief hysteresis countdown below - which
    /// exists to survive one-frame focus blips without the keyboard
    /// flickering away - genuinely needs to force a wakeup each frame so the
    /// counter advances instead of stalling until unrelated input arrives.
    fn keyboard_input_needed(&mut self, ctx: &Context) -> bool {
        if ctx.egui_wants_keyboard_input() {
            self.needed = 20;
            true
        } else {
            self.needed = self.needed.saturating_sub(1);
            if self.needed > 0 {
                ctx.request_repaint();
            }
            self.needed > 0
        }
    }
}

/// LOCAL ADDITION (experiment 026): `b` over `a` at `t`, keeping `a`'s alpha.
fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), a.a())
}

#[allow(dead_code)]
/// Trim the text to the maximum length, and add ellipsis if needed.
fn trim_text(text: &str, max_length: usize) -> String {
    let mut result = String::new();
    for (n, c) in text.chars().enumerate() {
        if n >= max_length {
            result.push('…');
            break;
        }
        result.push(c);
    }
    result
}
