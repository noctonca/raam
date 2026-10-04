//! The web's `TileSource`: the engine's fetch thread (raam-engine
//! fetch.rs) redone for a host without threads, over the bundled sample
//! photos. The host calls `pump` once per loop pass; it does in steps what
//! the fetch thread does in its loop — plan the next collage from the
//! queue with `collage::pick`, park it, then hand its tiles over one at a
//! time, waiting on each photo's download — and the slideshow sees the same
//! seam either way.
//!
//! The catalogue (sizes and faces, `www/faces.json`; titles,
//! `www/photos/credits.json`) is compiled in, so planning needs no
//! request, as the engine plans from the DB before fetching pixels. The
//! pixels come from the browser: an `<img>` per photo, requested when its
//! plan is parked, decoded to RGBA through a 2D canvas when its tile is
//! due, and dropped again after, so at most one decoded preview is held
//! at once (the browser's HTTP cache keeps the bytes). The web offers no
//! clips.

use raam_core::collage::{self, Orientation, Rect};
use raam_core::source::{TileSource, shrink_to_cover};
use raam_core::{clock, collage::LARGEST_LAYOUT};
use raam_model::{
    AssetId, CurationKey, Focus, MediaItem, Photo, Plan, RemoteId, SourceKind, TilePhoto,
};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::time::Duration;
use wasm_bindgen::JsCast;
use web_sys::{CanvasRenderingContext2d, HtmlImageElement};

const FACES: &str = include_str!("../www/faces.json");
const CREDITS: &str = include_str!("../www/photos/credits.json");

/// One bundled photo, as the catalogue lists it.
pub struct Sample {
    /// The curation key (Fill/Fit and Hide act on it).
    pub key: String,
    pub title: String,
    pub file: String,
    pub width: u32,
    pub height: u32,
    pub focus: Focus,
}

/// The catalogue: `faces.json` joined with the credits' titles. A photo
/// with a malformed entry is a build defect, so this panics on one.
pub fn catalogue() -> Vec<Sample> {
    let faces: serde_json::Value = serde_json::from_str(FACES).expect("faces.json");
    let credits: serde_json::Value = serde_json::from_str(CREDITS).expect("credits.json");
    let title = |file: &str| {
        credits
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["file"] == file)
            .and_then(|c| c["title"].as_str())
            .unwrap_or(file)
            .to_string()
    };
    faces
        .as_array()
        .expect("faces.json is a list")
        .iter()
        .map(|e| {
            let file = e["file"].as_str().expect("faces.json: file").to_string();
            let boxes: Vec<(f32, (f32, f32))> = e["faces"]
                .as_array()
                .expect("faces.json: faces")
                .iter()
                .map(|b| {
                    let v = |i: usize| b[i].as_f64().expect("faces.json: box") as f32;
                    let (x1, y1, x2, y2) = (v(0), v(1), v(2), v(3));
                    ((x2 - x1) * (y2 - y1), ((x1 + x2) / 2.0, (y1 + y2) / 2.0))
                })
                .collect();
            Sample {
                key: format!("sample-{}", file.trim_end_matches(".jpg")),
                title: title(&file),
                width: e["width"].as_u64().expect("faces.json: width") as u32,
                height: e["height"].as_u64().expect("faces.json: height") as u32,
                focus: Focus::from_faces(&boxes),
                file,
            }
        })
        .collect()
}

/// A photo's download.
enum Image {
    Idle,
    Loading(HtmlImageElement),
}

/// The plan whose tiles are being handed over.
struct Build {
    plan: Plan,
    rects: Vec<Rect>,
    next: usize,
    is_request: bool,
    started: Duration,
}

struct State {
    samples: Vec<Sample>,
    images: Vec<Image>,
    hidden: HashSet<String>,
    enabled: bool,
    max_group: usize,
    screen: (i32, i32, i32),
    // The keyed shuffle (fetch.rs's `Queue`, without the saved resume
    // point: a page load starts a new pass).
    seed: u64,
    cursor: Option<u64>,
    counter: u64,
    seq: u64,
    // The slot the seam reads (fetch.rs's `Slot`).
    plan: Option<Plan>,
    tile: Option<TilePhoto>,
    request: Option<Plan>,
    failed: Option<u64>,
    pending: bool,
    build: Option<Build>,
    empty_logged: bool,
}

pub struct WebSource {
    state: RefCell<State>,
    /// Something landed for the render loop since the last `pump`.
    woke: Cell<bool>,
    decoder: CanvasRenderingContext2d,
}

impl WebSource {
    /// `screen` is (width, height, separator half-margin) in px.
    pub fn new(max_group: usize, screen: (i32, i32, i32)) -> Result<Self, String> {
        let samples = catalogue();
        log::info!(
            "sample catalogue: {} photos, {} with faces",
            samples.len(),
            samples.iter().filter(|s| s.focus.face.is_some()).count()
        );
        let doc = web_sys::window()
            .and_then(|w| w.document())
            .ok_or("no document")?;
        let canvas: web_sys::HtmlCanvasElement = doc
            .create_element("canvas")
            .map_err(|e| format!("{e:?}"))?
            .dyn_into()
            .map_err(|_| "not a canvas")?;
        let attrs = web_sys::ContextAttributes2d::new();
        attrs.set_will_read_frequently(true);
        let decoder = canvas
            .get_context_with_context_options("2d", &attrs)
            .map_err(|e| format!("2d context: {e:?}"))?
            .ok_or("no 2d context")?
            .dyn_into::<CanvasRenderingContext2d>()
            .map_err(|_| "not a 2d context")?;
        let images = samples.iter().map(|_| Image::Idle).collect();
        Ok(Self {
            state: RefCell::new(State {
                samples,
                images,
                hidden: HashSet::new(),
                enabled: true,
                max_group,
                screen,
                seed: new_seed(0),
                cursor: None,
                counter: 0,
                seq: 0,
                plan: None,
                tile: None,
                request: None,
                failed: None,
                pending: false,
                build: None,
                empty_logged: false,
            }),
            woke: Cell::new(false),
            decoder,
        })
    }

    /// Titles by curation key, for the settings' Hidden list.
    pub fn title(&self, key: &str) -> Option<String> {
        let s = self.state.borrow();
        s.samples
            .iter()
            .find(|p| p.key == key)
            .map(|p| p.title.clone())
    }

    pub fn len(&self) -> usize {
        self.state.borrow().samples.len()
    }

    pub fn set_max_group(&self, max: usize) {
        self.state.borrow_mut().max_group = max;
    }

    pub fn set_hidden(&self, key: &str, hidden: bool) {
        let mut s = self.state.borrow_mut();
        if hidden {
            s.hidden.insert(key.to_string());
        } else {
            s.hidden.remove(key);
        }
    }

    pub fn set_enabled(&self, on: bool) {
        self.state.borrow_mut().enabled = on;
    }

    /// Whether a plan, tile or failure landed since the last call: the
    /// render loop's wake (the engine's `Waker`).
    pub fn take_woke(&self) -> bool {
        self.woke.replace(false)
    }

    /// One step of the fetch loop, once per loop pass.
    pub fn pump(&self) {
        let mut s = self.state.borrow_mut();
        if s.build.is_some() {
            self.hand_over(&mut s);
            if s.build.is_some() {
                return;
            }
        }
        let requested = s.request.take();
        if requested.is_none() && s.pending {
            return;
        }
        let is_request = requested.is_some();
        let Some(mut plan) = requested.or_else(|| plan_next(&mut s)) else {
            return;
        };
        s.seq += 1;
        plan.seq = s.seq;
        for a in &plan.assets {
            let i = (a.asset.get() - 1) as usize;
            if matches!(s.images[i], Image::Idle) {
                s.images[i] = Image::Loading(load(&s.samples[i].file));
            }
        }
        let (w, h, margin) = s.screen;
        let rects = match plan.layout {
            Some(i) => collage::LAYOUTS[i].rects(w, h, margin),
            None => vec![Rect { x: 0, y: 0, w, h }],
        };
        s.plan = Some(plan.clone());
        s.tile = None;
        s.pending = true;
        s.build = Some(Build {
            plan,
            rects,
            next: 0,
            is_request,
            started: clock::now(),
        });
        self.woke.set(true);
    }

    /// Parks the plan's next tile once its photo has arrived.
    fn hand_over(&self, s: &mut State) {
        let b = s.build.as_ref().expect("a build");
        let seq = b.plan.seq;
        // The last tile was taken: done, whatever the render loop did with
        // the collage since.
        if s.tile.is_none() && b.next == b.plan.assets.len() {
            log::info!(
                "plan {seq} ({} tiles) fetched in {:?}",
                b.plan.assets.len(),
                clock::elapsed(b.started)
            );
            s.build = None;
            return;
        }
        // A Prev request, or the render loop dropped this plan (`consumed`
        // without showing it).
        if s.request.is_some() || !s.pending {
            if !s.pending {
                s.tile = None;
            }
            log::info!("plan {seq} abandoned");
            s.build = None;
            return;
        }
        if s.tile.is_some() {
            return;
        }
        let (slot, rect, is_request) = (b.next, b.rects[b.next], b.is_request);
        let entry = b.plan.assets[slot].clone();
        let i = (entry.asset.get() - 1) as usize;
        let Image::Loading(img) = &s.images[i] else {
            // Parking the plan asks for every photo in it; this one was
            // decoded for an earlier plan since, so ask again.
            s.images[i] = Image::Loading(load(&s.samples[i].file));
            return;
        };
        if !img.complete() {
            return;
        }
        let img = img.clone();
        s.images[i] = Image::Idle;
        let fetch_start = clock::now();
        let decoded = if img.natural_width() == 0 {
            Err(format!("{} didn't load", s.samples[i].file))
        } else {
            self.decode(&img)
        };
        match decoded {
            Ok((width, height, rgba)) => {
                let sample = &s.samples[i];
                if (width, height) != (sample.width, sample.height) {
                    log::warn!(
                        "asset {} is {width}x{height}, the catalogue says {}x{}",
                        entry.asset,
                        sample.width,
                        sample.height
                    );
                }
                let mut photo = Photo {
                    asset_id: entry.asset,
                    key: entry.key.clone(),
                    width,
                    height,
                    rgba,
                    face_focal: sample.focus.face,
                    fill_centre: sample.focus.centre,
                    video: None,
                };
                let preview = (photo.width, photo.height);
                let resize_time = shrink_to_cover(&mut photo, rect);
                log::info!(
                    "fetched tile {slot}/{} {}x{} (preview {}x{}, shrunk in {resize_time:?}) sample:{} (requested={is_request}) in {:?} - face {:?}, fill centre ({:.3},{:.3})",
                    s.build.as_ref().map_or(0, |b| b.plan.assets.len()),
                    photo.width,
                    photo.height,
                    preview.0,
                    preview.1,
                    entry.asset,
                    clock::elapsed(fetch_start),
                    photo.face_focal,
                    photo.fill_centre.0,
                    photo.fill_centre.1,
                );
                s.tile = Some(TilePhoto { seq, slot, photo });
                if let Some(b) = s.build.as_mut() {
                    b.next += 1;
                }
            }
            Err(e) => {
                log::error!("fetch of asset {} (plan {seq}) failed: {e}", entry.asset);
                s.failed = Some(seq);
                s.pending = false;
                s.build = None;
                log::info!("plan {seq} abandoned");
            }
        }
        self.woke.set(true);
    }

    /// The browser's decode, read back as RGBA.
    fn decode(&self, img: &HtmlImageElement) -> Result<(u32, u32, Vec<u8>), String> {
        let (w, h) = (img.natural_width(), img.natural_height());
        let canvas = self.decoder.canvas().ok_or("the decoder has no canvas")?;
        canvas.set_width(w);
        canvas.set_height(h);
        self.decoder
            .draw_image_with_html_image_element(img, 0.0, 0.0)
            .map_err(|e| format!("drawImage: {e:?}"))?;
        let data = self
            .decoder
            .get_image_data(0.0, 0.0, w as f64, h as f64)
            .map_err(|e| format!("getImageData: {e:?}"))?;
        // Release the decode canvas's backing store until the next tile.
        canvas.set_width(0);
        canvas.set_height(0);
        Ok((w, h, data.data().0))
    }
}

impl TileSource for WebSource {
    fn take_plan(&self) -> Option<Plan> {
        self.state.borrow_mut().plan.take()
    }

    fn take_tile(&self, seq: u64) -> Option<TilePhoto> {
        let mut s = self.state.borrow_mut();
        match &s.tile {
            Some(t) if t.seq == seq => s.tile.take(),
            Some(_) => {
                s.tile = None;
                None
            }
            None => None,
        }
    }

    fn take_failed(&self) -> Option<u64> {
        self.state.borrow_mut().failed.take()
    }

    fn consumed(&self) {
        self.state.borrow_mut().pending = false;
    }

    fn request(&self, plan: Plan) {
        let mut s = self.state.borrow_mut();
        s.request = Some(plan);
        s.failed = None;
    }

    fn tile_is_clip(&self, _seq: u64) -> bool {
        false
    }

    fn set_skip_videos(&self, _skip: bool) {
        // No clips here to pass over.
    }
}

fn load(file: &str) -> HtmlImageElement {
    let img = HtmlImageElement::new().expect("an <img>");
    img.set_src(&format!("photos/{file}"));
    img
}

/// The next collage from the queue, as the fetch thread plans it: the
/// window after the cursor (reshuffled at the end of a pass), cut to the
/// collage max, and `collage::pick`'s layout for its orientations.
fn plan_next(s: &mut State) -> Option<Plan> {
    let mut order: Vec<usize> = (0..s.samples.len())
        .filter(|&i| s.enabled && !s.hidden.contains(&s.samples[i].key))
        .collect();
    if order.is_empty() {
        if !s.empty_logged {
            log::warn!("nothing to show (every sample photo is hidden or the folder is off)");
            s.empty_logged = true;
        }
        return None;
    }
    s.empty_logged = false;
    let seed = s.seed;
    order.sort_by_key(|&i| rank(seed, i as i64 + 1));
    let start = |s: &State, order: &[usize]| match s.cursor {
        Some(c) => order.partition_point(|&i| rank(s.seed, i as i64 + 1) <= c),
        None => 0,
    };
    if start(s, &order) >= order.len() {
        s.counter += 1;
        s.seed = new_seed(s.counter);
        s.cursor = None;
        let seed = s.seed;
        order.sort_by_key(|&i| rank(seed, i as i64 + 1));
        log::info!(
            "queue reshuffled ({} photos, seed {:016x})",
            order.len(),
            s.seed
        );
    }
    let from = start(s, &order);
    let max = s.max_group.max(1);
    let window = &order[from..(from + max.min(LARGEST_LAYOUT)).min(order.len())];
    let orientations: Vec<Orientation> = window
        .iter()
        .map(|&i| Orientation::of(s.samples[i].width, s.samples[i].height))
        .collect();
    let counter = &mut s.counter;
    let choice = collage::pick(&orientations, max, |n| {
        *counter += 1;
        pseudo_random_below(n, *counter)
    });
    if let Some(&last) = window[..choice.consumed().min(window.len())].last() {
        s.cursor = Some(rank(s.seed, last as i64 + 1));
    }
    let assets: Vec<MediaItem> = choice
        .slot_to_item
        .iter()
        .map(|&w| {
            let i = window[w];
            let p = &s.samples[i];
            MediaItem {
                asset: AssetId::new(i as i64 + 1),
                key: CurationKey::new(p.key.clone()),
                source: SourceKind::Local,
                remote_id: RemoteId::new(p.file.clone()),
                width: p.width,
                height: p.height,
                video: false,
                location: Some(format!("photos/{}", p.file)),
            }
        })
        .collect();
    log::info!(
        "planned {} from window {:?} (max {max}): slots {:?}",
        choice
            .layout
            .map_or("1 (single)", |i| collage::LAYOUTS[i].name),
        orientations
            .iter()
            .map(|o| if *o == Orientation::Landscape {
                'L'
            } else {
                'P'
            })
            .collect::<String>(),
        assets
            .iter()
            .map(|a| format!("sample:{}:{}x{}", a.asset, a.width, a.height))
            .collect::<Vec<_>>(),
    );
    Some(Plan {
        seq: 0,
        layout: choice.layout,
        assets,
    })
}

/// splitmix64 of the seed and the asset id (fetch.rs's keyed shuffle).
fn rank(seed: u64, asset: i64) -> u64 {
    let mut z = seed ^ (asset as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn new_seed(counter: u64) -> u64 {
    // Wall time, so each page load shuffles differently.
    rank(clock::wall().as_nanos() as u64, counter as i64)
}

/// fetch.rs's xorshift64 draw, seeded from the clock plus a counter.
fn pseudo_random_below(n: u32, counter: u64) -> u32 {
    let nanos = clock::now().as_nanos() as u64;
    let mut x = (nanos ^ counter.wrapping_mul(0x9E3779B97F4A7C15)) | 1;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    (x % n.max(1) as u64) as u32
}
