//! The widget gallery: every token and component in the current theme, in
//! the list/detail layout the real settings use (sections on the left, the
//! chosen one on the right). The design system's QA surface, not one of the
//! product's screens. Host-agnostic: it takes a `ProbeInfo` from the host
//! and hands back `Request`s, and never touches GL or Android.
use crate::icons;
use crate::kit::{self, ButtonKind, ListItem, Tone, Trailing};
use crate::theme::{self, Scheme, TextMode, Type, scheme, size, space};
use egui::{Align, Color32, CornerRadius, Rect, Sense, Theme, Ui, vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Settings,
    Components,
    Colours,
    Type,
    Icons,
    Targets,
    Probe,
}

impl Page {
    pub const ALL: [Page; 7] = [
        Page::Settings,
        Page::Components,
        Page::Colours,
        Page::Type,
        Page::Icons,
        Page::Targets,
        Page::Probe,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Page::Settings => "settings",
            Page::Components => "components",
            Page::Colours => "colours",
            Page::Type => "type",
            Page::Icons => "icons",
            Page::Targets => "targets",
            Page::Probe => "probe",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Page::Settings => "Settings sample",
            Page::Components => "Components",
            Page::Colours => "Colour roles",
            Page::Type => "Type scale",
            Page::Icons => "Icons",
            Page::Targets => "Touch targets",
            Page::Probe => "Probe",
        }
    }

    fn icon(self) -> char {
        match self {
            Page::Settings => icons::SETTINGS,
            Page::Components => icons::TUNE,
            Page::Colours => icons::PALETTE,
            Page::Type => icons::FORMAT_SIZE,
            Page::Icons => icons::IMAGE,
            Page::Targets => icons::TOUCH_APP,
            Page::Probe => icons::INFO,
        }
    }

    pub fn from_name(s: &str) -> Option<Page> {
        Page::ALL.into_iter().find(|p| p.name() == s)
    }
}

/// What the host measured, shown on the Probe page.
#[derive(Default, Clone)]
pub struct ProbeInfo {
    pub text_mode: Option<TextMode>,
    pub subpixel: bool,
    pub ppp: f32,
    pub atlas: [usize; 2],
    pub atlas_fill: f32,
    pub atlas_uploads: u32,
    /// Empty until the first measured switch.
    pub last_switch: String,
    pub gl_max_texture: i32,
    /// 0 when the host can't tell (no /proc/meminfo, as on macOS).
    pub mem_free_kb: u64,
    pub fps: f32,
}

/// Changes only the host can make (they touch egui's options or the painter).
#[derive(Debug, Clone, Copy)]
pub enum Request {
    Theme(Theme),
    TextMode(TextMode),
    Subpixel(bool),
    Ppp(f32),
}

/// One row of the touch-target test.
struct TargetRow {
    px: f32,
    lit: usize,
    hits: u32,
    wrong: u32,
    misses: u32,
}

pub struct Gallery {
    pub page: Page,
    switches: [bool; 3],
    check: bool,
    radio: usize,
    slider: f32,
    text: String,
    seg: usize,
    chips: [bool; 4],
    combo: usize,
    dialog: bool,
    // Kit inputs, beside egui's own for comparison.
    field_url: String,
    field_empty: String,
    field_key: String,
    field_bad: String,
    kit_slider: f32,
    kit_steps: f32,
    kit_checks: [bool; 3],
    // Settings sample.
    interval: f32,
    transition: usize,
    transition_dialog: bool,
    ken_burns: bool,
    clock_24h: bool,
    theme_seg: usize,
    targets: Vec<TargetRow>,
    rng: u32,
}

const TRANSITIONS: [&str; 5] = ["Rotate", "Crossfade", "Cube", "Slide", "None"];

impl Default for Gallery {
    fn default() -> Self {
        Self {
            page: Page::Settings,
            switches: [true, false, true],
            check: true,
            radio: 0,
            slider: 30.0,
            text: "http://immich.local:2283".into(),
            seg: 1,
            chips: [true, false, true, false],
            combo: 0,
            dialog: false,
            field_url: "http://immich.local:2283".into(),
            field_empty: String::new(),
            field_key: "secret-api-key".into(),
            field_bad: "immich local".into(),
            kit_slider: 30.0,
            kit_steps: 60.0,
            kit_checks: [true, false, true],
            interval: 30.0,
            transition: 0,
            transition_dialog: false,
            ken_burns: true,
            clock_24h: true,
            theme_seg: 1,
            targets: [32.0, 40.0, 48.0, 56.0]
                .into_iter()
                .map(|px| TargetRow {
                    px,
                    lit: 0,
                    hits: 0,
                    wrong: 0,
                    misses: 0,
                })
                .collect(),
            rng: 0x2545_f491,
        }
    }
}

pub fn draw(ui: &mut Ui, g: &mut Gallery, info: &ProbeInfo) -> Vec<Request> {
    let mut req = Vec::new();
    let s = scheme(ui);
    let dark = ui.visuals().dark_mode;
    g.theme_seg = if dark { 1 } else { 0 };

    // The window's own background: the detail pane's rounded corner shows
    // it, and the host clears to black.
    ui.painter().rect_filled(ui.max_rect(), 0.0, s.surface);

    // No separator lines: M3 separates the panes by their surface colours.
    egui::Panel::top("top")
        .exact_size(size::TOP_BAR)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(s.surface))
        .show(ui, |ui| {
            kit::top_bar(ui, "Design system", |ui| {
                let icon = if dark {
                    icons::LIGHT_MODE
                } else {
                    icons::DARK_MODE
                };
                if kit::icon_button(ui, icon, false).clicked() {
                    req.push(Request::Theme(if dark {
                        Theme::Light
                    } else {
                        Theme::Dark
                    }));
                }
            });
        });

    egui::Panel::left("nav")
        .exact_size(size::LIST_PANE)
        .resizable(false)
        .show_separator_line(false)
        .frame(
            egui::Frame::new()
                .fill(s.surface)
                .inner_margin(egui::Margin::symmetric(space::M as i8, 0)),
        )
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for p in Page::ALL {
                if kit::nav_item(ui, p.icon(), p.label(), g.page == p).clicked() {
                    g.page = p;
                }
            }
        });

    egui::CentralPanel::default()
        .frame(
            egui::Frame::new()
                .fill(s.surface_container_low)
                .corner_radius(CornerRadius {
                    nw: theme::shape::L,
                    ..Default::default()
                })
                .inner_margin(egui::Margin::same(space::XL as i8)),
        )
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(g.page.name())
                .auto_shrink([false, false])
                .show(ui, |ui| match g.page {
                    Page::Settings => settings(ui, g, &mut req),
                    Page::Components => components(ui, g),
                    Page::Colours => colours(ui, &s),
                    Page::Type => type_scale(ui, &s),
                    Page::Icons => icons_page(ui, &s),
                    Page::Targets => targets(ui, g, &s),
                    Page::Probe => probe(ui, info, &mut req),
                });
        });

    if g.dialog {
        g.dialog = kit::dialog(ui.ctx(), "demo-dialog", "Forget this server?", |ui| {
            ui.label(theme::text(
                "The frame keeps its cached photos, but stops syncing until you add a server again.",
                Type::BodyMedium,
            ));
            ui.add_space(space::XL);
            let mut keep = true;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::button(ui, ButtonKind::Text, None, "Forget").clicked() {
                    keep = false;
                }
                if kit::button(ui, ButtonKind::Text, None, "Cancel").clicked() {
                    keep = false;
                }
            });
            keep
        });
    }
    if g.transition_dialog {
        let mut pick = g.transition;
        g.transition_dialog = kit::dialog(ui.ctx(), "transition-dialog", "Transition", |ui| {
            for (i, name) in TRANSITIONS.iter().enumerate() {
                ui.radio_value(&mut pick, i, theme::text(*name, Type::BodyLarge));
            }
            ui.add_space(space::L);
            let mut keep = true;
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if kit::button(ui, ButtonKind::Text, None, "OK").clicked() {
                    keep = false;
                }
            });
            keep
        });
        g.transition = pick;
    }
    req
}

fn page_title(ui: &mut Ui, title: &str, sub: &str) {
    kit::page_title(ui, title, sub);
}

/// A realistic slice of the settings, to judge the kit in context.
fn settings(ui: &mut Ui, g: &mut Gallery, req: &mut Vec<Request>) {
    page_title(
        ui,
        "Settings sample",
        "How the real settings will look in this layout.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    kit::section_header(ui, "Slideshow");
    let interval = format!("{:.0} s", g.interval);
    kit::list_item(
        ui,
        ListItem::new("Photo interval")
            .icon(icons::TIMER)
            .trailing(Trailing::Value(&interval)),
    );
    if kit::list_item(
        ui,
        ListItem::new("Transition")
            .icon(icons::ANIMATION)
            .trailing(Trailing::Value(TRANSITIONS[g.transition])),
    )
    .clicked()
    {
        g.transition_dialog = true;
    }
    kit::list_item(
        ui,
        ListItem::new("Ken Burns")
            .icon(icons::SLIDESHOW)
            .supporting("Slow pan and zoom, towards faces when Immich knows them")
            .trailing(Trailing::Switch(&mut g.ken_burns)),
    );
    kit::section_header(ui, "Display");
    let mut seg = g.theme_seg;
    let theme_row =
        ListItem::new("Theme")
            .icon(icons::BRIGHTNESS_6)
            .trailing(Trailing::Segmented {
                selected: &mut seg,
                options: &["Light", "Dark"],
                seg_w: 120.0,
            });
    if kit::list_item(ui, theme_row).changed() {
        req.push(Request::Theme(if seg == 1 {
            Theme::Dark
        } else {
            Theme::Light
        }));
    }
    kit::list_item(
        ui,
        ListItem::new("Clock")
            .icon(icons::SCHEDULE)
            .trailing(Trailing::Value("Simple")),
    );
    // No icon: a digital clock is illegible at 24 px, and Clock has the
    // clock face. The empty slot keeps the text on the text edge.
    kit::list_item(
        ui,
        ListItem::new("24-hour clock")
            .blank_icon()
            .trailing(Trailing::Switch(&mut g.clock_24h)),
    );
    kit::section_header(ui, "Photos");
    kit::list_item(
        ui,
        ListItem::new("Immich albums")
            .icon(icons::PHOTO_ALBUM)
            .supporting("2 albums · 227 photos")
            .trailing(Trailing::Chevron),
    );
    // Directly under the row it's about, 8 either side (a group's gap).
    ui.add_space(space::S);
    kit::note(
        ui,
        Tone::Warning,
        icons::WARNING,
        "“Iceland” is not on the server any more, so it plays nothing.",
    );
    ui.add_space(space::S);
    kit::list_item(
        ui,
        ListItem::new("Local folder")
            .icon(icons::FOLDER)
            .supporting("/sdcard/Pictures/Frame · 6 photos")
            .trailing(Trailing::Chevron),
    );
    kit::section_header(ui, "Server");
    kit::list_item(
        ui,
        ListItem::new("Immich server")
            .icon(icons::DNS)
            .supporting("http://immich.local:2283 · online")
            .trailing(Trailing::Chevron),
    );
    ui.add_space(space::XXL);
}

/// A component page's group: on the content edge, 16 in from the container
/// edge where its section header's text also starts.
fn on_content_edge<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: space::L as i8,
            right: space::L as i8,
            ..Default::default()
        })
        .show(ui, add)
        .inner
}

fn components(ui: &mut Ui, g: &mut Gallery) {
    // Fields and sliders in the first column share this width.
    const FIELD_W: f32 = 360.0;
    page_title(
        ui,
        "Components",
        "Kit components (M3), then egui's own widgets styled by the theme.",
    );
    // Rows of one group are 8 apart; section headers make the 24 between.
    ui.spacing_mut().item_spacing.y = space::S;
    kit::section_header(ui, "Buttons");
    on_content_edge(ui, |ui| {
        ui.horizontal(|ui| {
            kit::button(ui, ButtonKind::Filled, Some(icons::REFRESH), "Sync now");
            kit::button(ui, ButtonKind::Tonal, None, "Tonal");
            kit::button(ui, ButtonKind::Outlined, None, "Outlined");
            kit::button(ui, ButtonKind::Text, None, "Text");
        });
        ui.horizontal(|ui| {
            ui.add_enabled_ui(false, |ui| {
                kit::button(ui, ButtonKind::Filled, None, "Disabled");
                kit::button(ui, ButtonKind::Outlined, None, "Disabled");
            });
            if kit::button(ui, ButtonKind::Tonal, Some(icons::DELETE), "Open a dialog").clicked() {
                g.dialog = true;
            }
        });
        // The first icon, not its 48 px target, goes on the content edge: the
        // row is laid out 12 px to the left of the space it takes, since
        // negative space would widen the page instead.
        let (row, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), size::TOUCH), Sense::hover());
        let overhang = (size::TOUCH - size::ICON) / 2.0;
        let mut icons_row = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(row.translate(vec2(-overhang, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        for (i, ic) in [
            icons::PLAY_ARROW,
            icons::PAUSE,
            icons::SKIP_NEXT,
            icons::FAVORITE,
        ]
        .into_iter()
        .enumerate()
        {
            kit::icon_button(&mut icons_row, ic, i == 3);
        }
    });
    kit::section_header(ui, "Choices");
    on_content_edge(ui, |ui| {
        ui.horizontal(|ui| {
            kit::switch(ui, &mut g.switches[0]);
            kit::switch(ui, &mut g.switches[1]);
            ui.add_enabled_ui(false, |ui| kit::switch(ui, &mut g.switches[2]));
        });
        kit::segmented(ui, &mut g.seg, &["Off", "Simple", "Detailed"], 150.0);
        ui.horizontal(|ui| {
            for (i, name) in ["Favorites", "Iceland", "Lisbon", "Documents"]
                .iter()
                .enumerate()
            {
                if kit::chip(ui, name, g.chips[i]).clicked() {
                    g.chips[i] = !g.chips[i];
                }
            }
        });
    });
    kit::section_header(ui, "Inputs");
    on_content_edge(ui, |ui| {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::L;
            kit::TextField::new(&mut g.field_url, "Server URL")
                .icon(icons::DNS)
                .width(FIELD_W)
                .show(ui);
            kit::TextField::new(&mut g.field_empty, "Album name")
                .hint("e.g. Digital Frame")
                .supporting("Tap to see the hint")
                .width(300.0)
                .show(ui);
        });
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = space::L;
            kit::TextField::new(&mut g.field_key, "API key")
                .icon(icons::KEY)
                .password(true)
                .width(FIELD_W)
                .show(ui);
            // Postel's Law: a missing scheme is added, not an error. Only
            // what can't be a host name at all is.
            let v = g.field_bad.trim();
            let bad = v.is_empty() || v.contains(char::is_whitespace);
            let help = if bad {
                "Not a web address".to_owned()
            } else if v.starts_with("http://") || v.starts_with("https://") {
                "Looks right".to_owned()
            } else {
                format!("Will use http://{v}")
            };
            kit::TextField::new(&mut g.field_bad, "Server URL")
                .error(bad)
                .supporting(&help)
                .width(300.0)
                .show(ui);
        });
        ui.add_enabled_ui(false, |ui| {
            let mut off = "Set by the server".to_owned();
            kit::TextField::new(&mut off, "Disabled")
                .width(FIELD_W)
                .show(ui);
        });
    });
    // Sliders and checkboxes get their own sections: 8 under the last field,
    // they read as more fields.
    kit::section_header(ui, "Sliders");
    on_content_edge(ui, |ui| {
        ui.spacing_mut().slider_width = FIELD_W;
        kit::slider(
            ui,
            "Photo interval",
            &mut g.kit_slider,
            5.0..=120.0,
            None,
            |v| format!("{v:.0} s"),
        );
        kit::slider(
            ui,
            "Brightness, steps of 10",
            &mut g.kit_steps,
            0.0..=100.0,
            Some(10.0),
            |v| format!("{v:.0}%"),
        );
        ui.add_enabled_ui(false, |ui| {
            let mut v = 40.0;
            kit::slider(ui, "Disabled", &mut v, 0.0..=100.0, None, |v| {
                format!("{v:.0}")
            });
        });
    });
    kit::section_header(ui, "Checkboxes");
    on_content_edge(ui, |ui| {
        ui.horizontal(|ui| {
            // The boxes' targets overhang 15 to the left, so 24 between a
            // label and the next box leaves 9 between targets.
            ui.spacing_mut().item_spacing.x = space::XL;
            kit::checkbox(ui, &mut g.kit_checks[0], "Show captions");
            kit::checkbox(ui, &mut g.kit_checks[1], "Skip videos");
            ui.add_enabled_ui(false, |ui| {
                kit::checkbox(ui, &mut g.kit_checks[2], "Disabled")
            });
            kit::checkbox(ui, &mut g.kit_checks[1], "");
        });
    });
    kit::section_header(ui, "Notes");
    kit::note(
        ui,
        Tone::Info,
        icons::INFO,
        "Info note on secondary-container.",
    );
    kit::note(
        ui,
        Tone::Error,
        icons::ERROR,
        "Can't reach the server (17:34), showing the saved list.",
    );
    // Deliberately stock egui, only styled by the theme: left as it is.
    kit::section_header(ui, "egui widgets, themed");
    on_content_edge(ui, |ui| {
        ui.checkbox(&mut g.check, "Checkbox (egui)");
        ui.horizontal(|ui| {
            ui.radio_value(&mut g.radio, 0, "Radio A");
            ui.radio_value(&mut g.radio, 1, "Radio B");
        });
        ui.add(
            egui::Slider::new(&mut g.slider, 5.0..=120.0)
                .suffix(" s")
                .text("Slider"),
        );
        ui.add(egui::TextEdit::singleline(&mut g.text).hint_text("Server URL"));
        egui::ComboBox::from_label("Combo box")
            .selected_text(TRANSITIONS[g.combo])
            .show_ui(ui, |ui| {
                for (i, t) in TRANSITIONS.iter().enumerate() {
                    ui.selectable_value(&mut g.combo, i, *t);
                }
            });
        ui.horizontal(|ui| {
            let _ = ui.button("egui Button");
            let _ = ui.add(egui::Button::selectable(true, "Selected"));
            ui.hyperlink_to("Hyperlink", "https://immich.app");
        });
        ui.separator();
        ui.label(egui::RichText::new("Weak text (egui weak colour)").weak());
        ui.colored_label(ui.visuals().warn_fg_color, "warn_fg_color");
        ui.colored_label(ui.visuals().error_fg_color, "error_fg_color");
    });
    ui.add_space(space::XXL);
}

/// Room below the last thing on a page, as on the settings sample.
fn page_end(ui: &mut Ui) {
    ui.add_space(space::XXL);
}

/// How a colour role is shown on the Colours page.
enum Swatch {
    /// A fill role, with the role its text uses on it.
    Fill(Color32, Color32),
    /// A fill that sits close to the page (surfaces): outlined so its edge shows.
    Surface(Color32),
    /// A stroke role (outlines): never a text background, so it's shown as a
    /// stroke and a swatch on surface, with on-surface text.
    Stroke(Color32),
}

fn colours(ui: &mut Ui, s: &Scheme) {
    page_title(
        ui,
        "Colour roles",
        "Seed #3F7A74, TonalSpot. Each tile is the role with its on-colour text; outlines, which never carry text, are shown as strokes.",
    );
    use Swatch::*;
    let tiles: [(&str, Swatch); 24] = [
        ("primary", Fill(s.primary, s.on_primary)),
        (
            "primaryContainer",
            Fill(s.primary_container, s.on_primary_container),
        ),
        ("secondary", Fill(s.secondary, s.on_secondary)),
        (
            "secondaryContainer",
            Fill(s.secondary_container, s.on_secondary_container),
        ),
        ("tertiary", Fill(s.tertiary, s.on_tertiary)),
        (
            "tertiaryContainer",
            Fill(s.tertiary_container, s.on_tertiary_container),
        ),
        ("error", Fill(s.error, s.on_error)),
        (
            "errorContainer",
            Fill(s.error_container, s.on_error_container),
        ),
        ("warning", Fill(s.warning, s.on_warning)),
        (
            "warningContainer",
            Fill(s.warning_container, s.on_warning_container),
        ),
        (
            "inverseSurface",
            Fill(s.inverse_surface, s.inverse_on_surface),
        ),
        // inversePrimary is text on inverseSurface, so the pair shows reversed.
        ("inversePrimary", Fill(s.inverse_primary, s.inverse_surface)),
        ("surfaceDim", Surface(s.surface_dim)),
        ("surface", Surface(s.surface)),
        ("surfaceBright", Surface(s.surface_bright)),
        ("containerLowest", Surface(s.surface_container_lowest)),
        ("containerLow", Surface(s.surface_container_low)),
        ("container", Surface(s.surface_container)),
        ("containerHigh", Surface(s.surface_container_high)),
        ("containerHighest", Surface(s.surface_container_highest)),
        // The text roles, as fills with surface text on them.
        ("onSurface", Fill(s.on_surface, s.surface)),
        ("onSurfaceVariant", Fill(s.on_surface_variant, s.surface)),
        ("outline", Stroke(s.outline)),
        ("outlineVariant", Stroke(s.outline_variant)),
    ];
    // Tiles fill the pane from the container edge (384) to its far edge, in
    // whole pixels, with S between them.
    const MIN_W: f32 = 200.0;
    const TILE_H: f32 = 72.0;
    let avail = ui.available_width();
    let cols = ((avail + space::S) / (MIN_W + space::S)).floor().max(1.0) as usize;
    let tile_w = ((avail - space::S * (cols - 1) as f32) / cols as f32).floor();
    ui.spacing_mut().item_spacing = vec2(space::S, space::S);
    for row in tiles.chunks(cols) {
        ui.horizontal(|ui| {
            for (name, swatch) in row {
                let (r, _) = ui.allocate_exact_size(vec2(tile_w, TILE_H), Sense::hover());
                let (bg, fg, stroke) = match *swatch {
                    Fill(bg, fg) => (bg, fg, None),
                    Surface(bg) => (bg, s.on_surface, Some(s.outline_variant)),
                    Stroke(c) => (s.surface, s.on_surface, Some(c)),
                };
                let p = ui.painter();
                let corner = CornerRadius::same(theme::shape::M);
                p.rect_filled(r, corner, bg);
                if let Some(c) = stroke {
                    p.rect_stroke(
                        r,
                        corner,
                        egui::Stroke::new(1.0, c),
                        egui::StrokeKind::Inside,
                    );
                }
                let role = match *swatch {
                    Fill(c, _) | Surface(c) | Stroke(c) => c,
                };
                if let Stroke(c) = *swatch {
                    // The colour at a readable size, on the trailing side, inset
                    // L all round like the text.
                    let side = TILE_H - 2.0 * space::L;
                    let sw = Rect::from_min_size(
                        egui::pos2(r.right() - space::L - side, r.top() + space::L),
                        vec2(side, side),
                    );
                    p.rect_filled(sw, CornerRadius::same(theme::shape::S), c);
                }
                // Two M3 line boxes (20 + 16) with XS between, inset L all round:
                // each line's cap centre on its line box's centre.
                let x = r.left() + space::L;
                kit::text_on(
                    ui.painter(),
                    x,
                    Align::Min,
                    r.top() + space::L + 10.0,
                    name,
                    Type::LabelLarge,
                    fg,
                );
                let hex = format!("#{:02X}{:02X}{:02X}", role.r(), role.g(), role.b());
                kit::text_on(
                    ui.painter(),
                    x,
                    Align::Min,
                    r.top() + space::L + 20.0 + space::XS + 8.0,
                    hex,
                    Type::BodySmall,
                    fg,
                );
            }
        });
    }
    page_end(ui);
}

fn type_scale(ui: &mut Ui, s: &Scheme) {
    page_title(
        ui,
        "Type scale",
        "M3 roles in Roboto. Title M/S and labels use Roboto Medium.",
    );
    // Spaced by the ink, not the line boxes: at 57 px a line box's leading
    // alone is bigger than the gaps, so equal line-box gaps looked unequal.
    // Each caption's baseline is S above its sample's cap top, and a
    // sample's descender line is L above the next caption's cap top.
    let caption_cap = (Type::LabelSmall.spec().1 * kit::ROBOTO_CAP).round();
    ui.spacing_mut().item_spacing.y = 0.0;
    for (i, ty) in Type::ALL.into_iter().enumerate() {
        let (name, px, lh, medium) = ty.spec();
        let (cap, descent) = (
            (px * kit::ROBOTO_CAP).round(),
            (px * kit::ROBOTO_DESCENT).round(),
        );
        let h = caption_cap
            + space::S
            + cap
            + descent
            + if i + 1 < Type::ALL.len() {
                space::L
            } else {
                0.0
            };
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
        let caption = format!(
            "{name} {px:.0}/{lh:.0}{}",
            if medium { " medium" } else { "" }
        );
        let y = r.top() + caption_cap;
        kit::on_baseline(
            ui.painter(),
            r.left(),
            Align::Min,
            y,
            caption,
            Type::LabelSmall.font(),
            s.on_surface_variant,
        );
        kit::on_baseline(
            ui.painter(),
            r.left(),
            Align::Min,
            y + space::S + cap,
            "Lisbon 20° · Sep 25, 17:34",
            ty.font(),
            s.on_surface,
        );
    }
    page_end(ui);
}

fn icons_page(ui: &mut Ui, s: &Scheme) {
    page_title(
        ui,
        "Icons",
        "The Material Symbols Rounded subset: each icon outlined, then filled.",
    );
    // Whole-pixel cells across the pane, so the columns are evenly spaced
    // and every icon lands on a pixel.
    const MIN_CELL: f32 = 120.0;
    let avail = ui.available_width();
    let cols = (avail / MIN_CELL).floor().max(1.0) as usize;
    let cell = vec2((avail / cols as f32).floor(), 72.0);
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    for row in icons::ALL.chunks(cols) {
        ui.horizontal(|ui| {
            for (name, ch) in row {
                let (r, _) = ui.allocate_exact_size(cell, Sense::hover());
                // The pair: 24 + S + 24, centred on the cell, over its label.
                let c = egui::pos2(r.center().x, r.top() + space::XL);
                let d = (size::ICON + space::S) / 2.0;
                kit::icon_on(
                    ui.painter(),
                    c.x - d,
                    Align::Center,
                    c.y,
                    *ch,
                    egui::FontId::new(size::ICON, theme::ICONS.clone()),
                    s.on_surface,
                );
                kit::icon_on(
                    ui.painter(),
                    c.x + d,
                    Align::Center,
                    c.y,
                    *ch,
                    egui::FontId::new(size::ICON, theme::ICONS_FILL.clone()),
                    s.primary,
                );
                kit::text_on(
                    ui.painter(),
                    r.center().x,
                    Align::Center,
                    r.bottom() - space::M,
                    name,
                    Type::LabelSmall,
                    s.on_surface_variant,
                );
                let shown = egui::Rangef::new(
                    kit::icon_ink(c.y, size::ICON).min,
                    kit::caps(r.bottom() - space::M, Type::LabelSmall).max,
                );
                kit::mark_visual(ui, r, shown);
            }
        });
    }
    page_end(ui);
}

fn next_rand(g: &mut Gallery) -> u32 {
    // xorshift32: the lit target only needs to move unpredictably.
    let mut x = g.rng;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    g.rng = x;
    x
}

/// Tap the lit target in each row. A tap on another target is "wrong", a
/// tap in the row's gaps is a "miss". This page is for a person's finger;
/// `adb input tap` is exact and would prove nothing.
fn targets(ui: &mut Ui, g: &mut Gallery, s: &Scheme) {
    page_title(
        ui,
        "Touch targets",
        "Tap the lit target in each row, as fast as is comfortable. 8 px gaps, like dense UI.",
    );
    const N: usize = 8;
    // The targets start this far into the row: the label column, from the
    // content edge (400), holds "hit 99 · wrong 99 · miss 99".
    const TARGETS_X: f32 = space::L + 224.0;
    ui.spacing_mut().item_spacing.y = 0.0;
    for ri in 0..g.targets.len() {
        let px = g.targets[ri].px;
        // The targets plus S above and below: every row is L from the next,
        // target edge to target edge, whatever its size (a fixed pitch
        // left the small rows' gaps wider). A tap in that band is a miss.
        let row_h = px + space::L;
        let (row, row_resp) =
            ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
        let target = |i: usize| {
            Rect::from_min_size(
                row.left_top() + vec2(TARGETS_X + i as f32 * (px + space::S), (row_h - px) / 2.0),
                vec2(px, px),
            )
        };
        let mut hit: Option<usize> = None;
        for i in 0..N {
            let r = target(i);
            let resp = ui.interact(r, ui.id().with(("t", ri, i)), Sense::click());
            if resp.clicked() {
                hit = Some(i);
            }
            let lit = g.targets[ri].lit == i;
            let fill = if lit {
                s.primary
            } else {
                s.surface_container_highest
            };
            ui.painter()
                .rect_filled(r, CornerRadius::same(theme::shape::S), fill);
        }
        if let Some(i) = hit {
            let lit = g.targets[ri].lit;
            let lit_c = target(lit).center();
            let origin = ui.input(|inp| inp.pointer.press_origin());
            let off = origin.map(|o| o - lit_c).unwrap_or_default();
            log::info!(
                "target {px:.0}px: lit {lit}, clicked {i} | press {:?}, offset from lit centre ({:+.0}, {:+.0}) pt",
                origin.map(|o| (o.x.round(), o.y.round())),
                off.x,
                off.y
            );
        }
        let t = &mut g.targets[ri];
        // Two lines as one block centred on the targets, like a two-line
        // list row: the size, then the counts.
        let (x, cy) = (row.left() + space::L, row.center().y);
        kit::text_on(
            ui.painter(),
            x,
            Align::Min,
            cy - 10.0,
            format!("{px:.0} px"),
            Type::LabelLarge,
            s.on_surface,
        );
        kit::text_on(
            ui.painter(),
            x,
            Align::Min,
            cy + 10.0,
            format!("hit {} · wrong {} · miss {}", t.hits, t.wrong, t.misses),
            Type::BodyMedium,
            s.on_surface_variant,
        );
        match hit {
            Some(i) if i == t.lit => {
                t.hits += 1;
                let r = next_rand(g) as usize % N;
                g.targets[ri].lit = r;
            }
            Some(_) => t.wrong += 1,
            None if row_resp.clicked() => t.misses += 1,
            None => {}
        }
    }
    ui.add_space(space::XL);
    ui.horizontal(|ui| {
        ui.add_space(space::L);
        if kit::button(
            ui,
            ButtonKind::Outlined,
            Some(icons::RESTART_ALT),
            "Reset counts",
        )
        .clicked()
        {
            for t in &mut g.targets {
                t.hits = 0;
                t.wrong = 0;
                t.misses = 0;
            }
        }
    });
    let log: Vec<String> = g
        .targets
        .iter()
        .map(|t| format!("{}px {}/{}/{}", t.px, t.hits, t.wrong, t.misses))
        .collect();
    ui.ctx()
        .data_mut(|d| d.insert_temp(egui::Id::new("targets.log"), log.join(", ")));
    page_end(ui);
}

/// An empty or unknown value, per the QA checklist: never a blank or a fake zero.
const NONE: &str = "—";

fn probe(ui: &mut Ui, info: &ProbeInfo, req: &mut Vec<Request>) {
    let s = scheme(ui);
    page_title(
        ui,
        "Probe",
        "Live numbers from the host, and the switches under test.",
    );
    ui.spacing_mut().item_spacing.y = 0.0;
    kit::card(ui, |ui| {
        // Keys on the card's content edge (400), values on one column: the
        // key column fits "GL_MAX_TEXTURE_SIZE" (about 150) with room.
        const KEY_W: f32 = 200.0;
        // Line boxes of 24 with S between them, as rows of one group.
        const ROW_H: f32 = 24.0 + space::S;
        ui.spacing_mut().item_spacing.y = 0.0;
        let row = |ui: &mut Ui, k: &str, v: String| {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
            kit::text_on(
                ui.painter(),
                r.left(),
                Align::Min,
                r.center().y,
                k,
                Type::LabelLarge,
                s.on_surface_variant,
            );
            let v = if v.is_empty() { NONE.to_owned() } else { v };
            kit::text_on(
                ui.painter(),
                r.left() + KEY_W,
                Align::Min,
                r.center().y,
                v,
                Type::BodyLarge,
                s.on_surface,
            );
        };
        let known = |ok: bool, v: String| if ok { v } else { NONE.to_owned() };
        row(
            ui,
            "Text mode",
            info.text_mode.map_or(NONE, |m| m.name()).to_string(),
        );
        row(
            ui,
            "Sub-pixel binning",
            if info.subpixel { "on" } else { "off" }.to_string(),
        );
        row(ui, "pixels_per_point", format!("{:.2}", info.ppp));
        row(
            ui,
            "Font atlas",
            known(
                info.atlas[0] > 0,
                format!(
                    "{}×{} ({:.0}% full, {} full uploads)",
                    info.atlas[0],
                    info.atlas[1],
                    info.atlas_fill * 100.0,
                    info.atlas_uploads
                ),
            ),
        );
        row(
            ui,
            "GL_MAX_TEXTURE_SIZE",
            known(info.gl_max_texture > 0, info.gl_max_texture.to_string()),
        );
        row(ui, "Last theme switch", info.last_switch.clone());
        row(
            ui,
            "MemFree",
            known(
                info.mem_free_kb > 0,
                format!("{:.1} MB", info.mem_free_kb as f32 / 1024.0),
            ),
        );
    });
    // Controls on the content edge (400), under their section headers.
    let indented = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        ui.horizontal(|ui| {
            ui.add_space(space::L);
            add(ui);
        });
    };
    kit::section_header(ui, "Text mode");
    let modes = [
        TextMode::EguiDefault,
        TextMode::Off,
        TextMode::Shader,
        TextMode::Boost,
    ];
    let mut i = modes
        .iter()
        .position(|m| Some(*m) == info.text_mode)
        .unwrap_or(0);
    indented(ui, &mut |ui| {
        if kit::segmented(ui, &mut i, &["egui", "off", "shader", "boost"], 110.0) {
            req.push(Request::TextMode(modes[i]));
        }
    });
    ui.add_space(space::S);
    let mut sub = info.subpixel;
    if kit::list_item(
        ui,
        ListItem::new("Sub-pixel binning").trailing(Trailing::Switch(&mut sub)),
    )
    .changed()
    {
        req.push(Request::Subpixel(sub));
    }
    kit::section_header(ui, "Scale");
    let scales = [1.0, 1.25, 1.5];
    let mut j = scales
        .iter()
        .position(|v| (*v - info.ppp).abs() < 0.01)
        .unwrap_or(0);
    indented(ui, &mut |ui| {
        if kit::segmented(ui, &mut j, &["1.0", "1.25", "1.5"], 110.0) {
            req.push(Request::Ppp(scales[j]));
        }
    });
    kit::section_header(ui, "Text over light and dark");
    for (i, (bg, fg)) in [
        (s.surface, s.on_surface),
        (s.primary, s.on_primary),
        (s.inverse_surface, s.inverse_on_surface),
    ]
    .into_iter()
    .enumerate()
    {
        if i > 0 {
            ui.add_space(space::S);
        }
        egui::Frame::new()
            .fill(bg)
            .corner_radius(CornerRadius::same(theme::shape::M))
            .inner_margin(egui::Margin::symmetric(space::L as i8, space::M as i8))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(
                    theme::text(
                        "The quick brown fox jumps over the lazy dog 0123456789",
                        Type::BodyMedium,
                    )
                    .color(fg),
                );
                ui.label(
                    theme::text(
                        "Small print at 11 px: labelSmall, the thinnest strokes we use",
                        Type::LabelSmall,
                    )
                    .color(fg),
                );
            });
    }
    page_end(ui);
}
