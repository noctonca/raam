//! 018's chrome (touch menu, trimmed settings, `egui_keyboard` text entry)
//! redrawn as translucent Areas over the live slideshow instead of an opaque
//! CentralPanel. The chrome only records intent (`Actions`, settings
//! values); lib.rs applies it to the slideshow pipeline every frame.
//! 020 adds Frameo's per-photo Fill/Fit button and the two scaling settings.
//! 021 adds "Collage max" and "Gap colour"; the Fill/Fit button acts on the
//! tapped (highlighted) tile.
//! 022 adds "Clock overlay" (Off / Top right / Bottom left) and "24-hour clock".
//! 023 adds "Sleep schedule" with its sleep and wake times.
//! 024 adds a "Photos" section: the two sources' switches, the local
//! folder, the Immich cache (size, cap, clear), rescan/sync buttons, the
//! hidden photos (each can be unhidden) and the curation export; and
//! "Hide" in the menu, undoable for a few seconds. Every setting is now
//! saved (lib.rs sends them to the writer thread).
//! 025 replaces 019's album stub button with an album picker in "Photos":
//! every Immich album with a checkbox and its count, any number picked.
//! 027 adds "Videos": Frameo's playback choice, sound and volume, and what
//! the library holds; the menu has no Fill/Fit for a clip.
use egui::{Align2, Color32, Context, Frame, RichText};
use raam_model::limits::{AUDIO_DELAY_RANGE, CAP_CHOICES_MB, LARGEST_LAYOUT};
use raam_model::{
    ClockStyle, FitBackground, GapColour, ScaleMode, Settings, Stats, TransitionChoice,
    VideoPlayback,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Menu,
    Settings,
}

#[derive(Default)]
pub struct Actions {
    pub next: bool,
    pub prev: bool,
    pub close: bool,
    pub toggle_scale: bool,
    pub clear_cache: bool,
    pub rescan: bool,
    pub sync_now: bool,
    pub hide: bool,
    pub undo_hide: bool,
    pub unhide: Option<String>,
    pub export: bool,
    /// 025: (Immich album id, picked).
    pub select_album: Vec<(String, bool)>,
}

pub struct AppState {
    pub screen: Screen,
    pub settings: Settings,
    pub paused: bool,
    pub keyboard: egui_keyboard::Keyboard,
    pub actions: Actions,
    /// The shown photo's scaling, set by lib.rs each frame; labels the
    /// Fill/Fit button with the one it switches to, as Frameo does.
    pub shown_scale: Option<ScaleMode>,
    /// 027: the tapped tile is a clip (Hide, but no Fill/Fit).
    pub shown_video: bool,
    /// The library's counts, copied in by lib.rs before each egui run.
    pub library: Stats,
    /// Seconds left to undo the last Hide, set by lib.rs.
    pub undo_secs: Option<u64>,
    /// 025: picks not yet seen back in `library.albums`, so a checkbox
    /// doesn't flick back while the writer thread commits.
    pub pending_albums: std::collections::HashMap<String, bool>,
}

impl AppState {
    pub fn new(server_url: &str, api_key: &str) -> Self {
        Self {
            screen: Screen::Menu,
            settings: Settings::defaults(server_url, api_key),
            paused: false,
            keyboard: egui_keyboard::Keyboard::default(),
            actions: Actions::default(),
            shown_scale: None,
            shown_video: false,
            library: Default::default(),
            undo_secs: None,
            pending_albums: Default::default(),
        }
    }

    /// Back to a closed overlay's resting state. The keyboard is replaced
    /// rather than kept: its 20-frame focus hysteresis would otherwise flash
    /// it up for a few frames the next time the overlay opens.
    pub fn reset_on_close(&mut self) {
        self.screen = Screen::Menu;
        self.keyboard = egui_keyboard::Keyboard::default();
    }
}

fn panel_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_black_alpha(170))
        .corner_radius(16.0)
        .inner_margin(20.0)
}

pub fn draw(ctx: &Context, state: &mut AppState, status: &str) {
    state.keyboard.pump_events(ctx);
    match state.screen {
        Screen::Menu => draw_menu(ctx, state, status),
        Screen::Settings => draw_settings(ctx, state),
    }
    state.keyboard.show(ctx);
}

fn draw_menu(ctx: &Context, state: &mut AppState, status: &str) {
    egui::Area::new(egui::Id::new("menu"))
        .anchor(Align2::CENTER_BOTTOM, [0.0, -32.0])
        .show(ctx, |ui| {
            panel_frame().show(ui, |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(menu_button(if state.paused {
                            "▶ Resume"
                        } else {
                            "⏸ Pause"
                        }))
                        .clicked()
                    {
                        state.paused = !state.paused;
                    }
                    if ui.add(menu_button("⏮ Prev")).clicked() {
                        state.actions.prev = true;
                    }
                    if ui.add(menu_button("⏭ Next")).clicked() {
                        state.actions.next = true;
                    }
                    if let Some(mode) = state.shown_scale {
                        let label = match mode {
                            ScaleMode::Fill => "Fit to frame",
                            ScaleMode::Fit => "Fill frame",
                        };
                        if ui.add(menu_button(label)).clicked() {
                            state.actions.toggle_scale = true;
                        }
                    }
                    if let Some(secs) = state.undo_secs {
                        if ui
                            .add(menu_button(&format!("Undo hide ({secs})")))
                            .clicked()
                        {
                            state.actions.undo_hide = true;
                        }
                    } else if (state.shown_scale.is_some() || state.shown_video)
                        && ui.add(menu_button("Hide")).clicked()
                    {
                        state.actions.hide = true;
                    }
                    if ui.add(menu_button("⚙ Settings")).clicked() {
                        state.screen = Screen::Settings;
                    }
                    if ui.add(menu_button("Close")).clicked() {
                        state.actions.close = true;
                    }
                });
                ui.add_space(8.0);
                ui.label(RichText::new(status).color(Color32::from_gray(200)));
            });
        });
}

fn menu_button(text: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).size(20.0)).min_size(egui::vec2(96.0, 64.0))
}

fn draw_settings(ctx: &Context, state: &mut AppState) {
    let safe = state.keyboard.safe_rect(ctx);
    let width = (safe.width() - 64.0).min(760.0);
    egui::Area::new(egui::Id::new("settings"))
        .anchor(Align2::CENTER_TOP, [0.0, 24.0])
        .show(ctx, |ui| {
            panel_frame().show(ui, |ui| {
                ui.set_width(width);
                ui.spacing_mut().slider_width = (width - 120.0).max(200.0);
                ui.spacing_mut().interact_size.y = 40.0;
                ui.horizontal(|ui| {
                    if ui.add(menu_button("⬅ Back")).clicked() {
                        state.screen = Screen::Menu;
                    }
                    ui.heading("Settings");
                });
                ui.add_space(8.0);
                // Keeps the panel above the on-screen keyboard when it is up.
                let max_h = (safe.height() - 24.0 - 64.0 - 48.0 - 40.0).max(120.0);
                // 025: without `min_scrolled_height` the ScrollArea shrank to
                // about 290px inside the Area (019-024's "below the fold"),
                // too short for the album list.
                egui::ScrollArea::vertical()
                    .max_height(max_h)
                    .min_scrolled_height(max_h)
                    .show(ui, |ui| {
                        photos_section(ui, state);
                        ui.add_space(12.0);
                        videos_section(ui, state);
                        ui.add_space(12.0);

                        ui.label("Clock overlay:");
                        ui.horizontal(|ui| {
                            for style in [
                                ClockStyle::Off,
                                ClockStyle::TopRight,
                                ClockStyle::BottomLeft,
                            ] {
                                ui.selectable_value(
                                    &mut state.settings.clock_style,
                                    style,
                                    style.label(),
                                );
                            }
                            ui.add_space(16.0);
                            ui.checkbox(&mut state.settings.clock_24h, "24-hour clock");
                        });
                        ui.add_space(12.0);

                        ui.checkbox(
                            &mut state.settings.sleep_enabled,
                            "Sleep schedule (screen off at night)",
                        );
                        if state.settings.sleep_enabled {
                            time_slider(ui, "Sleep at", &mut state.settings.sleep_min);
                            time_slider(ui, "Wake at", &mut state.settings.wake_min);
                        }
                        ui.add_space(12.0);

                        ui.label("Immich server URL:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.server_url)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(8.0);
                        ui.label("API key:");
                        ui.add(
                            egui::TextEdit::singleline(&mut state.settings.api_key)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                        ui.label(
                            RichText::new(
                                "Server and key are applied and saved when the menu closes.",
                            )
                            .small()
                            .color(Color32::from_gray(180)),
                        );
                        ui.add_space(12.0);

                        ui.label("Slideshow interval:");
                        ui.add(
                            egui::Slider::new(&mut state.settings.interval_secs, 5.0..=120.0)
                                .suffix(" s"),
                        );
                        ui.add_space(8.0);

                        ui.label(format!(
                            "Collage max photos (default for this screen: {}):",
                            state.settings.screen_default_max
                        ));
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut state.settings.collage_max, 1, "Off");
                            for n in 2..=LARGEST_LAYOUT {
                                ui.selectable_value(
                                    &mut state.settings.collage_max,
                                    n,
                                    n.to_string(),
                                );
                            }
                        });
                        ui.label("Gap colour:");
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut state.settings.gap_colour,
                                GapColour::Black,
                                "Black",
                            );
                            ui.selectable_value(
                                &mut state.settings.gap_colour,
                                GapColour::White,
                                "White",
                            );
                        });
                        ui.add_space(8.0);

                        ui.label("Transition style:");
                        egui::ComboBox::from_id_salt("transition_style")
                            .selected_text(state.settings.transition.label())
                            .show_ui(ui, |ui| {
                                for t in TransitionChoice::ALL {
                                    ui.selectable_value(
                                        &mut state.settings.transition,
                                        t,
                                        t.label(),
                                    );
                                }
                            });
                        ui.add_space(8.0);
                        ui.checkbox(&mut state.settings.ken_burns_enabled, "Ken Burns pan/zoom");
                        ui.add_space(8.0);
                        ui.checkbox(&mut state.settings.fill_by_default, "Fill frame by default");
                        ui.label(
                            RichText::new(if state.settings.fill_by_default {
                                "Photos fill the frame by default."
                            } else {
                                "Photos are fit to the frame by default."
                            })
                            .small()
                            .color(Color32::from_gray(180)),
                        );
                        ui.add_space(8.0);
                        ui.label("Fit background:");
                        ui.horizontal(|ui| {
                            ui.selectable_value(
                                &mut state.settings.fit_background,
                                FitBackground::Blurred,
                                "Blurred",
                            );
                            ui.selectable_value(
                                &mut state.settings.fit_background,
                                FitBackground::Black,
                                "Black",
                            );
                        });
                    });
            });
        });
}

fn photos_section(ui: &mut egui::Ui, state: &mut AppState) {
    let lib = state.library.clone();
    let mb = |b: i64| b as f64 / 1_048_576.0;
    let note = |ui: &mut egui::Ui, text: String| {
        ui.label(RichText::new(text).small().color(Color32::from_gray(180)));
    };
    ui.label("Photos:");
    let s = &mut state.settings;
    // Neither switch can turn off the only source that is on.
    ui.horizontal(|ui| {
        ui.add_enabled_ui(s.local_enabled, |ui| {
            ui.checkbox(&mut s.immich_enabled, "Immich albums")
        });
        ui.add_space(16.0);
        ui.add_enabled_ui(s.immich_enabled, |ui| {
            ui.checkbox(&mut s.local_enabled, "On-device folder")
        });
    });
    note(
        ui,
        format!(
            "Immich: {} photos, {} cached, {}. Prefetch: {}.",
            lib.immich_assets,
            lib.immich_cached,
            lib.immich_note,
            if lib.prefetch_note.is_empty() {
                "waiting"
            } else {
                &lib.prefetch_note
            },
        ),
    );
    note(
        ui,
        format!(
            "Folder {}: {} photos ({} ready), {}. In both sources (played once): {}.",
            lib.local_dir, lib.local_assets, lib.local_ready, lib.local_note, lib.shared
        ),
    );
    ui.label(format!(
        "Immich cache: {:.0} MB of {:.0} MB ({:.1} GB free on the frame)",
        mb(lib.cache_bytes),
        mb(lib.cap_bytes),
        lib.free_bytes as f64 / 1_073_741_824.0
    ));
    ui.horizontal(|ui| {
        ui.label("Cache size:");
        for cap in CAP_CHOICES_MB {
            let label = if cap >= 1024 {
                format!("{} GB", cap / 1024)
            } else {
                format!("{cap} MB")
            };
            ui.selectable_value(&mut s.cache_cap_mb, cap, label);
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Clear Immich cache").clicked() {
            state.actions.clear_cache = true;
        }
        if ui.button("Rescan folder").clicked() {
            state.actions.rescan = true;
        }
        if ui.button("Sync Immich now").clicked() {
            state.actions.sync_now = true;
        }
    });
    if s.immich_enabled {
        albums_list(ui, state, &lib);
    }
    ui.add_space(8.0);
    ui.label(format!("Hidden photos ({}):", lib.hidden.len()));
    for item in lib.hidden.iter().take(50) {
        ui.horizontal(|ui| {
            if ui.button("Unhide").clicked() {
                state.actions.unhide = Some(item.key.clone());
            }
            ui.label(&item.label);
        });
    }
    ui.horizontal(|ui| {
        if ui.button("Export curation").clicked() {
            state.actions.export = true;
        }
        note(
            ui,
            if lib.export_note.is_empty() {
                "Saved after every change.".to_string()
            } else {
                lib.export_note.clone()
            },
        );
    });
}

/// 027: Frameo's "Video playback" choice, sound and volume, and the
/// library's clips (how many can play here, and why not the others).
fn videos_section(ui: &mut egui::Ui, state: &mut AppState) {
    let lib = &state.library;
    let s = &mut state.settings;
    ui.label("Videos:");
    ui.horizontal(|ui| {
        for p in VideoPlayback::ALL {
            ui.selectable_value(&mut s.video_playback, p, p.label());
        }
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.video_sound, "Sound");
        ui.add_space(16.0);
        ui.add_enabled_ui(s.video_sound, |ui| {
            ui.label("Audio delay:");
            if ui.button("−").clicked() {
                s.audio_delay_ms = (s.audio_delay_ms - 10).max(AUDIO_DELAY_RANGE.0);
            }
            ui.label(format!("{:+} ms", s.audio_delay_ms));
            if ui.button("+").clicked() {
                s.audio_delay_ms = (s.audio_delay_ms + 10).min(AUDIO_DELAY_RANGE.1);
            }
        });
    });
    ui.horizontal(|ui| {
        ui.add_enabled_ui(s.video_sound, |ui| {
            ui.spacing_mut().slider_width = 300.0;
            let mut pct = (s.video_volume * 100.0).round();
            if ui
                .add(
                    egui::Slider::new(&mut pct, 0.0..=100.0)
                        .step_by(5.0)
                        .suffix(" %")
                        .text("Volume"),
                )
                .changed()
            {
                s.video_volume = pct / 100.0;
            }
        });
    });
    let mut note = format!(
        "{} clips in the albums and folder, {} ready to play.",
        lib.videos, lib.videos_ready
    );
    if lib.videos_unplayable > 0 {
        note.push_str(&format!(
            " {} can't be played on this frame ({}); Clear Immich cache tries them again.",
            lib.videos_unplayable,
            lib.unplayable_reasons.join("; ")
        ));
    }
    ui.label(RichText::new(note).small().color(Color32::from_gray(180)));
}

/// 025: every Immich album with a checkbox; any number can be picked and
/// their photos play as one shuffle. The list is the library thread's
/// (saved in the DB), so it shows offline too.
fn albums_list(ui: &mut egui::Ui, state: &mut AppState, lib: &Stats) {
    let note = |ui: &mut egui::Ui, text: &str, colour: Color32| {
        ui.label(RichText::new(text).small().color(colour));
    };
    let grey = Color32::from_gray(180);
    let warn = Color32::from_rgb(255, 196, 96);
    // Picks the list now agrees with are no longer pending.
    state.pending_albums.retain(|id, on| {
        lib.albums
            .iter()
            .any(|a| &a.remote_id == id && a.selected != *on)
    });
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Albums to show:");
        if ui.button("Refresh list").clicked() {
            state.actions.sync_now = true;
        }
    });
    note(ui, &format!("Album list: {}.", lib.albums_note), grey);
    if lib.albums.is_empty() {
        note(ui, "No albums yet: the list loads from the server.", grey);
        return;
    }
    // About how many previews the cache holds, from the average cached one
    // (about 300 KB here), so an album bigger than that (Recents, 10,966)
    // is flagged before it's picked: the rest would only play online.
    let avg = if lib.immich_cached > 0 {
        lib.cache_bytes / lib.immich_cached
    } else {
        300 * 1024
    };
    let fits = lib.cap_bytes / avg.max(1);
    let mut picked = 0;
    for album in &lib.albums {
        let mut on = state
            .pending_albums
            .get(&album.remote_id)
            .copied()
            .unwrap_or(album.selected);
        picked += on as usize;
        ui.horizontal(|ui| {
            if ui.checkbox(&mut on, &album.name).changed() {
                state.pending_albums.insert(album.remote_id.clone(), on);
                state
                    .actions
                    .select_album
                    .push((album.remote_id.clone(), on));
            }
            if album.missing {
                note(ui, "not on the server any more: plays nothing", warn);
            } else if album.asset_count > fits {
                note(
                    ui,
                    &format!(
                        "{} photos, more than the cache holds (about {fits})",
                        album.asset_count
                    ),
                    warn,
                );
            } else if on && album.selected && album.synced < album.asset_count {
                note(
                    ui,
                    &format!("{} of {} synced", album.synced, album.asset_count),
                    grey,
                );
            } else {
                note(ui, &format!("{} photos", album.asset_count), grey);
            }
        });
    }
    if picked == 0 {
        note(ui, "No album picked: Immich adds no photos.", warn);
    } else if lib.immich_assets > fits {
        note(
            ui,
            &format!(
                "The picked albums hold {} photos and about {fits} fit in the cache: the rest play only while the server can be reached.",
                lib.immich_assets
            ),
            warn,
        );
    }
}

/// A time of day in 15-minute steps, shown as HH:MM.
fn time_slider(ui: &mut egui::Ui, label: &str, min: &mut u32) {
    ui.horizontal(|ui| {
        ui.add_sized([90.0, 40.0], egui::Label::new(format!("{label}:")));
        // Leave room for the label and the value box on the same row.
        ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(200.0);
        ui.add(
            egui::Slider::new(min, 0..=(24 * 60 - 15))
                .step_by(15.0)
                .custom_formatter(|v, _| crate::schedule::fmt_hm(v as u32))
                .custom_parser(|_| None),
        );
    });
}
