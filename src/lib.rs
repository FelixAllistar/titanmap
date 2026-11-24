//! titanmap: a fast Path of Titans map. The same view runs as a desktop overlay (src/main.rs)
//! and in the browser (wasm `start`).
//!
//! Drawing: the base image is a 512 px tile pyramid; only the tiles on screen at the level that
//! matches the zoom are loaded. Every vector feature and every item dot goes into one `Shape`
//! per frame (a few draw calls no matter how many thousand dots), and only labels are elements.

pub mod data;
#[cfg(not(target_arch = "wasm32"))]
pub mod capture;
#[cfg(not(target_arch = "wasm32"))]
pub mod locate;

use std::collections::{HashMap, HashSet};

use data::{Layer, MapData, LAYERS, MAP_IDS, pretty};
use rune::tok::TextStyle;
use rune::{
    AppSpec, Color, Cx, Drag, El, Image, ImageCache, Key, KeyEvent, LineCap, MagnifyEvent, Phase, PointerEvent, ScrollSpec, Shape, Tx, Waker,
    WheelEvent, col, el, row, text,
};

// ------------------------------------------------------------------ palette

/// sRGB (0-1, as authored) to the linear light Rune's `Color` holds. Polynomial fit of the sRGB
/// curve, good to well under 1%, so palettes stay `const`.
pub const fn lin(c: f32) -> f32 { c * (c * (c * 0.305_306 + 0.682_171) + 0.012_523) }

pub const fn srgba(r: f32, g: f32, b: f32, a: f32) -> Color { Color { r: lin(r), g: lin(g), b: lin(b), a } }

pub const fn rgb(hex: u32) -> Color {
    srgba(((hex >> 16) & 255) as f32 / 255.0, ((hex >> 8) & 255) as f32 / 255.0, (hex & 255) as f32 / 255.0, 1.0)
}

const BG: Color = rgb(0x0b1418);
const PANEL: Color = srgba(0.06, 0.07, 0.08, 0.92);
const PANEL_EDGE: Color = srgba(1.0, 1.0, 1.0, 0.08);
const INK: Color = rgb(0xeceae4);
const INK_DIM: Color = rgb(0x9a9890);
const ACCENT: Color = rgb(0xe8b04a);
const HOVER: Color = srgba(1.0, 1.0, 1.0, 0.07);
const OUTLINE: Color = srgba(0.02, 0.02, 0.02, 0.85);

/// One colour per item category, in the order categories first appear; wraps if a map has more.
const CAT_COLORS: [u32; 15] = [
    0xff6b5a, 0x5ac8ff, 0xc9a26b, 0xf3d34a, 0xb78cff, 0x7ee081, 0xff9f43, 0x3ddbd9, 0xff7ac6, 0xa3e635, 0x60a5fa, 0xf472b6,
    0xfacc15, 0x94a3b8, 0xfb923c,
];

fn cat_color(i: usize) -> Color { rgb(CAT_COLORS[i % CAT_COLORS.len()]) }

fn style(size: f32, weight: f32) -> TextStyle { TextStyle::new(size, weight) }

// ------------------------------------------------------------------ state

#[derive(Clone, Copy, Debug)]
pub struct Cam {
    /// Centre of the view in full-resolution image pixels.
    pub c: [f32; 2],
    /// Screen points per image pixel.
    pub zoom: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct Player {
    pub map: usize,
    /// Full-resolution image pixels.
    pub at: [f32; 2],
}

type TileKey = (usize, u32, u32, u32);

pub struct State {
    pub maps: Vec<MapData>,
    pub cur: usize,
    cams: Vec<Option<Cam>>,
    vp: [f32; 2],
    pub layers: HashSet<Layer>,
    /// Enabled item types by Vulnona class name (shared across maps).
    pub items: HashSet<String>,
    expanded: HashSet<usize>,
    pub panel: bool,
    images: Option<ImageCache>,
    tiles: HashMap<TileKey, Image>,
    assets: String,
    hover: Option<usize>,
    pointer: Option<[f32; 2]>,
    drag_from: [f32; 2],
    pub player: Option<Player>,
    /// One line at the bottom: locate results and errors.
    pub status: String,
    /// Set by the desktop build: what the Locate button does.
    pub locate: Option<Box<dyn Fn()>>,
    /// Set by the desktop build: what the Hide button does.
    pub hide: Option<Box<dyn Fn()>>,
}

impl State {
    pub fn new() -> State {
        let maps: Vec<MapData> = MAP_IDS.iter().map(|id| MapData::load(id)).collect();
        let n = maps.len();
        State {
            maps,
            cur: 0,
            cams: vec![None; n],
            vp: [1280.0, 800.0],
            layers: [Layer::HomeCave, Layer::WayStone, Layer::Locations, Layer::Areas, Layer::Water, Layer::Caves].into_iter().collect(),
            items: HashSet::new(),
            expanded: HashSet::new(),
            panel: true,
            images: None,
            tiles: HashMap::new(),
            assets: default_assets(),
            hover: None,
            pointer: None,
            drag_from: [0.0, 0.0],
            player: None,
            status: String::new(),
            locate: None,
            hide: None,
        }
    }

    pub fn map(&self) -> &MapData { &self.maps[self.cur] }

    fn fit(&self) -> Cam {
        let m = self.map();
        let (w, h) = (m.image.w as f32, m.image.h as f32);
        Cam { c: [w / 2.0, h / 2.0], zoom: (self.vp[0] / w).min(self.vp[1] / h) * 0.95 }
    }

    pub fn cam(&self) -> Cam { self.cams[self.cur].unwrap_or_else(|| self.fit()) }

    fn min_zoom(&self) -> f32 { self.fit().zoom * 0.5 }

    pub fn set_cam(&mut self, mut cam: Cam) {
        cam.zoom = cam.zoom.clamp(self.min_zoom(), 6.0);
        let m = self.map();
        cam.c[0] = cam.c[0].clamp(0.0, m.image.w as f32);
        cam.c[1] = cam.c[1].clamp(0.0, m.image.h as f32);
        self.cams[self.cur] = Some(cam);
        self.ensure_tiles();
    }

    pub fn select_map(&mut self, i: usize) {
        if i < self.maps.len() && i != self.cur {
            self.cur = i;
            self.hover = None;
            self.ensure_tiles();
        }
    }

    pub fn select_map_id(&mut self, id: &str) {
        if let Some(i) = self.maps.iter().position(|m| m.id.eq_ignore_ascii_case(id)) { self.select_map(i); }
    }

    /// Image point -> screen point.
    fn to_screen(&self, cam: Cam, p: [f32; 2]) -> [f32; 2] {
        [(p[0] - cam.c[0]) * cam.zoom + self.vp[0] / 2.0, (p[1] - cam.c[1]) * cam.zoom + self.vp[1] / 2.0]
    }

    fn to_image(&self, cam: Cam, q: [f32; 2]) -> [f32; 2] {
        [(q[0] - self.vp[0] / 2.0) / cam.zoom + cam.c[0], (q[1] - self.vp[1] / 2.0) / cam.zoom + cam.c[1]]
    }

    pub fn zoom_about(&mut self, q: [f32; 2], factor: f32) {
        let cam = self.cam();
        let p = self.to_image(cam, q);
        let zoom = (cam.zoom * factor).clamp(self.min_zoom(), 6.0);
        // Keep p under q.
        let c = [p[0] - (q[0] - self.vp[0] / 2.0) / zoom, p[1] - (q[1] - self.vp[1] / 2.0) / zoom];
        self.set_cam(Cam { c, zoom });
    }

    /// Centre on the located player, zoomed in like the in-game map.
    pub fn focus_player(&mut self) {
        if let Some(p) = self.player {
            self.select_map(p.map);
            let zoom = self.cam().zoom.max(0.6);
            self.set_cam(Cam { c: p.at, zoom });
        }
    }

    /// The tile level for the current zoom, and the tiles on screen at that level.
    fn wanted_tiles(&self) -> (u32, Vec<(u32, u32)>) {
        let m = self.map();
        let cam = self.cam();
        let top = m.image.levels - 1;
        // Level z holds the image at 2^(z - top) scale; take the first with at least ~1.25 texels per point.
        let mut z = 0;
        while z < top && 2f32.powi(z as i32 - top as i32) < cam.zoom * 1.25 { z += 1; }
        let s = 2f32.powi(z as i32 - top as i32);
        let tile = m.image.tile as f32;
        let a = self.to_image(cam, [0.0, 0.0]);
        let b = self.to_image(cam, self.vp);
        let (lw, lh) = ((m.image.w as f32 * s).ceil(), (m.image.h as f32 * s).ceil());
        let (nx, ny) = ((lw / tile).ceil() as i64, (lh / tile).ceil() as i64);
        let x0 = ((a[0] * s / tile).floor() as i64).clamp(0, nx);
        let x1 = ((b[0] * s / tile).ceil() as i64).clamp(0, nx);
        let y0 = ((a[1] * s / tile).floor() as i64).clamp(0, ny);
        let y1 = ((b[1] * s / tile).ceil() as i64).clamp(0, ny);
        let mut v = Vec::new();
        for y in y0..y1 { for x in x0..x1 { v.push((x as u32, y as u32)); } }
        (z, v)
    }

    fn tile_path(&self, map: usize, z: u32, x: u32, y: u32) -> String {
        format!("{}/maps/{}/tiles/{z}/{x}_{y}.jpg", self.assets, self.maps[map].id)
    }

    /// Request the tiles the view needs (always the one-tile overview too) and drop far ones.
    pub fn ensure_tiles(&mut self) {
        let Some(cache) = self.images.as_ref() else { return };
        let (z, want) = self.wanted_tiles();
        let cur = self.cur;
        let mut keys: Vec<TileKey> = want.iter().map(|&(x, y)| (cur, z, x, y)).collect();
        keys.push((cur, 0, 0, 0));
        for k in keys {
            if !self.tiles.contains_key(&k) {
                let path = self.tile_path(k.0, k.1, k.2, k.3);
                let img = cache.file(path, 0.0, 1.0);
                self.tiles.insert(k, img);
            }
        }
        if self.tiles.len() > 400 {
            let keep: HashSet<TileKey> = want.iter().map(|&(x, y)| (cur, z, x, y)).collect();
            self.tiles.retain(|k, _| k.1 == 0 || keep.contains(k));
        }
    }

    pub fn item_visible(&self, it: &data::Item) -> bool {
        match it.layer {
            Layer::Item => self.items.contains(&it.f.class),
            l => self.layers.contains(&l),
        }
    }

    fn pick(&self, q: [f32; 2]) -> Option<usize> {
        let cam = self.cam();
        let mut best: Option<(f32, usize)> = None;
        for (i, it) in self.map().items.iter().enumerate() {
            if it.f.kind != "pt" || !self.item_visible(it) { continue; }
            let s = self.to_screen(cam, it.at);
            let d = (s[0] - q[0]).powi(2) + (s[1] - q[1]).powi(2);
            let r = if it.layer == Layer::Item { 8.0 } else { 12.0 };
            if d < r * r && best.map(|b| d < b.0).unwrap_or(true) { best = Some((d, i)); }
        }
        best.map(|b| b.1)
    }
}

impl Default for State {
    fn default() -> Self { State::new() }
}

/// Where tiles live: next to the executable, else the source tree (cargo run), else the page.
fn default_assets() -> String {
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("assets"))) {
            if dir.join("maps").exists() { return dir.to_string_lossy().into_owned(); }
        }
        let dev = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
        if dev.join("maps").exists() { return dev.to_string_lossy().into_owned(); }
        "assets".into()
    }
    #[cfg(target_arch = "wasm32")]
    { "assets".into() }
}

pub fn setup(s: &mut State, waker: Waker) {
    s.images = Some(ImageCache::new(waker));
    s.ensure_tiles();
}

pub fn poll(s: &mut State, _clock: f64, _tx: &mut Tx) -> bool {
    let before = s.tiles.len();
    s.ensure_tiles();
    s.tiles.len() != before
}

pub fn on_key(s: &mut State, e: &KeyEvent, _tx: &mut Tx) -> bool {
    let q = [s.vp[0] / 2.0, s.vp[1] / 2.0];
    match &e.key {
        Key::Text(t) if t == "+" || t == "=" => s.zoom_about(q, 1.4),
        Key::Text(t) if t == "-" || t == "_" => s.zoom_about(q, 1.0 / 1.4),
        Key::Text(t) if t == "1" => s.select_map(0),
        Key::Text(t) if t == "2" => s.select_map(1),
        Key::Text(t) if t == "3" => s.select_map(2),
        Key::Text(t) if t == "l" || t == "L" => s.panel = !s.panel,
        Key::Text(t) if t == "f" || t == "F" => s.focus_player(),
        Key::Escape => match &s.hide { Some(h) => h(), None => return false },
        _ => return false,
    }
    true
}

pub fn spec() -> AppSpec<State> {
    AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        min_size: Some((480.0, 360.0)),
        scenes: &[("located", scene_located)],
        ..AppSpec::new("titanmap", (1280.0, 800.0), State::new, view)
    }
}

/// Headless check (`--scene located`): Riparia, zoomed in on a located player, a few item layers on.
fn scene_located(s: &mut State) {
    s.select_map_id("Riparia");
    let at = s.map().to_px([222.0, 51.0]);
    s.player = Some(Player { map: s.cur, at });
    for t in ["Bone", "Berry", "Rock", "Branch", "Conch", "Reeds"] { s.items.insert(t.into()); }
    s.layers.insert(Layer::Caves);
    s.hover = None;
    s.set_cam(Cam { c: at, zoom: 0.9 });
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen::prelude::wasm_bindgen(start))]
pub fn start() {
    #[cfg(target_arch = "wasm32")]
    rune::run(spec());
}

// ------------------------------------------------------------------ view

pub fn view(s: &State, _cx: &mut Cx<State>) -> El {
    let map = map_canvas(s);
    let mut root = el().key("root").w_full().h_full().fill(BG).child(map).child(top_bar(s));
    if s.panel { root = root.child(side_panel(s)); }
    root.child(status_bar(s))
}

fn map_canvas(s: &State) -> El {
    let m = s.map();
    let cam = s.cam();
    let mut canvas = el().key("map").w_full().h_full().clip_soft(0.0)
        .on_layout(|s: &mut State, r| {
            if (s.vp[0] - r[2]).abs() > 0.5 || (s.vp[1] - r[3]).abs() > 0.5 {
                s.vp = [r[2], r[3]];
                s.ensure_tiles();
            }
        })
        .on_drag(|s: &mut State, d: &mut Drag, _tx: &mut Tx| {
            if d.phase == Phase::Down { s.drag_from = s.cam().c; }
            let cam = s.cam();
            let c = [s.drag_from[0] - d.dx / cam.zoom, s.drag_from[1] - d.dy / cam.zoom];
            s.set_cam(Cam { c, zoom: cam.zoom });
            s.hover = None;
        })
        .on_wheel(|s: &mut State, e: WheelEvent, _tx| {
            if e.precise && !e.mods.ctrl {
                // Trackpad: two-finger scroll pans.
                let cam = s.cam();
                s.set_cam(Cam { c: [cam.c[0] + e.dx / cam.zoom, cam.c[1] + e.dy / cam.zoom], zoom: cam.zoom });
            } else {
                let f = (-e.dy * 0.004).exp().clamp(0.5, 2.0);
                s.zoom_about([e.x, e.y], f);
            }
            true
        })
        .on_magnify(|s: &mut State, e: MagnifyEvent, _tx| {
            s.zoom_about([e.x, e.y], 1.0 + e.delta);
            true
        })
        .on_pointer_move_local(|s: &mut State, e: PointerEvent, _tx| {
            let q = [e.x, e.y];
            s.pointer = Some(q);
            s.hover = s.pick(q);
        })
        .on_hover_change(|s: &mut State, inside, _tx| {
            if !inside { s.pointer = None; s.hover = None; }
        });

    // Tiles: the overview underneath, then the level that matches the zoom.
    let top = m.image.levels - 1;
    let tile = m.image.tile as f32;
    let (z, want) = s.wanted_tiles();
    let mut draw_tile = |canvas: El, z: u32, x: u32, y: u32| -> El {
        let Some(img) = s.tiles.get(&(s.cur, z, x, y)) else { return canvas };
        let inv = 2f32.powi(top as i32 - z as i32); // image px per level px
        let x0 = x as f32 * tile * inv;
        let y0 = y as f32 * tile * inv;
        let x1 = ((x as f32 + 1.0) * tile * inv).min(m.image.w as f32);
        let y1 = ((y as f32 + 1.0) * tile * inv).min(m.image.h as f32);
        let a = s.to_screen(cam, [x0, y0]);
        let b = s.to_screen(cam, [x1, y1]);
        // Round outward so neighbouring tiles never leave a hairline gap.
        let (ax, ay) = (a[0].floor(), a[1].floor());
        canvas.child(el().key(format!("t{}_{z}_{x}_{y}", s.cur)).absolute(ax, ay).size(b[0].ceil() - ax, b[1].ceil() - ay).pixels(img.pixels()))
    };
    canvas = draw_tile(canvas, 0, 0, 0);
    if z > 0 { for &(x, y) in &want { canvas = draw_tile(canvas, z, x, y); } }

    canvas = canvas.child(el().key("vectors").absolute(0.0, 0.0).size(s.vp[0], s.vp[1]).shape(vectors(s, cam)));
    for label in labels(s, cam) { canvas = canvas.child(label); }
    if let Some(tip) = tooltip(s) { canvas = canvas.child(tip); }
    canvas
}

fn ellipse(c: [f32; 2], r: [f32; 2], rot_deg: f32, n: usize) -> Vec<[f32; 2]> {
    let (sn, cs) = rot_deg.to_radians().sin_cos();
    (0..n).map(|i| {
        let t = i as f32 / n as f32 * std::f32::consts::TAU;
        let (x, y) = (t.cos() * r[0], t.sin() * r[1]);
        [c[0] + x * cs - y * sn, c[1] + x * sn + y * cs]
    }).collect()
}

fn vectors(s: &State, cam: Cam) -> Shape {
    let m = s.map();
    let ppu = m.px_per_unit() * cam.zoom; // screen points per game unit
    let (w, h) = (s.vp[0], s.vp[1]);
    let on_screen = |p: [f32; 2], pad: f32| p[0] > -pad && p[1] > -pad && p[0] < w + pad && p[1] < h + pad;
    let mut shape = Shape::new();

    // Areas, water, caves, cliffs.
    for it in &m.items {
        if !s.item_visible(it) { continue; }
        let (color, width) = match it.layer {
            Layer::Areas => (srgba(1.0, 0.95, 0.8, 0.55), 1.5),
            Layer::Water => (srgba(0.45, 0.78, 1.0, 0.6), 1.25),
            Layer::Caves => (srgba(1.0, 0.62, 0.3, 0.9), (0.9 * ppu).clamp(1.5, 4.0)),
            Layer::Cliffs => (srgba(0.85, 0.85, 0.85, 0.7), 1.5),
            _ => continue,
        };
        match it.f.kind.as_str() {
            "line" => {
                for line in &it.lines {
                    let pts: Vec<[f32; 2]> = line.iter().map(|&p| s.to_screen(cam, p)).collect();
                    if !pts.iter().any(|&p| on_screen(p, 200.0)) && pts.len() < 3 { continue; }
                    let closed = pts.len() > 2 && line.first() == line.last();
                    shape = shape.stroke_with(pts, closed, width, LineCap::Round, color);
                }
            }
            "ell" => {
                let Some(r) = it.f.r else { continue };
                let c = s.to_screen(cam, it.at);
                let r = [r[0] * ppu, r[1] * ppu];
                if !on_screen(c, r[0].max(r[1]) + 10.0) { continue; }
                shape = shape.stroke_with(ellipse(c, r, it.f.rot, 40), true, width, LineCap::Round, color);
            }
            _ => {}
        }
    }

    // Item dots: one octagon each, darker rim for contrast on any terrain.
    let dot = (2.2 + cam.zoom * 3.0).clamp(3.0, 6.0);
    let oct = |c: [f32; 2], r: f32| -> Vec<[f32; 2]> { ellipse(c, [r, r], 22.5, 8) };
    for it in &m.items {
        if it.layer != Layer::Item || it.f.kind != "pt" || !s.items.contains(&it.f.class) { continue; }
        let c = s.to_screen(cam, it.at);
        if !on_screen(c, 10.0) { continue; }
        shape = shape.fill(oct(c, dot + 1.2), OUTLINE).fill(oct(c, dot), cat_color(it.cat));
    }

    // Home caves (square) and waystones (diamond) on top.
    for it in &m.items {
        if !matches!(it.layer, Layer::HomeCave | Layer::WayStone) || !s.item_visible(it) { continue; }
        let c = s.to_screen(cam, it.at);
        if !on_screen(c, 20.0) { continue; }
        let (r, fill, rot) = match it.layer {
            Layer::HomeCave => (7.5, if it.underwater { rgb(0x4aa8ff) } else { rgb(0xf08a3c) }, 45.0),
            _ => (8.5, if it.underwater { rgb(0x56e0d0) } else { rgb(0xb98cff) }, 0.0),
        };
        shape = shape.fill(ellipse(c, [r + 2.0, r + 2.0], rot, 4), OUTLINE).fill(ellipse(c, [r, r], rot, 4), fill);
    }

    // The player: an arrowhead with a ring, like the in-game marker.
    if let Some(p) = s.player.filter(|p| p.map == s.cur) {
        let c = s.to_screen(cam, p.at);
        shape = shape
            .fill(ellipse(c, [13.0, 13.0], 0.0, 32), srgba(0.2, 0.6, 1.0, 0.25))
            .stroke_with(ellipse(c, [13.0, 13.0], 0.0, 32), true, 2.0, LineCap::Round, rgb(0x5ab4ff))
            .fill(ellipse(c, [5.5, 5.5], 0.0, 20), rgb(0xffffff))
            .fill(ellipse(c, [3.5, 3.5], 0.0, 20), rgb(0x2f7df6));
    }
    shape
}

fn labels(s: &State, cam: Cam) -> Vec<El> {
    let m = s.map();
    let span = m.image.w as f32 * cam.zoom; // how wide the whole map is on screen
    let mut out = Vec::new();
    for (i, it) in m.items.iter().enumerate() {
        if !s.item_visible(it) { continue; }
        let (st, color, min_span) = match (it.f.kind.as_str(), it.layer) {
            ("txt", Layer::Areas) => (style(12.5, 650.0), rgb(0xfff4dc), 700.0),
            ("txt", Layer::Water) => (style(11.0, 500.0), rgb(0x9fd8ff), 2200.0),
            ("txt", Layer::Locations) => (style(11.0, 600.0), rgb(0xffd77a), 1800.0),
            ("txt", _) => (style(11.0, 500.0), INK, 2200.0),
            ("pt", Layer::HomeCave | Layer::WayStone) if !it.f.name.is_empty() => (style(11.0, 600.0), INK, 1400.0),
            _ => continue,
        };
        if span < min_span { continue; }
        let c = s.to_screen(cam, it.at);
        if c[0] < -100.0 || c[1] < -40.0 || c[0] > s.vp[0] + 100.0 || c[1] > s.vp[1] + 40.0 { continue; }
        let is_point = it.f.kind == "pt";
        let name = if is_point { it.f.name.clone() } else { it.f.name.replace('★', "").trim().to_string() };
        let y = if is_point { c[1] + 11.0 } else { c[1] - 9.0 };
        out.push(
            el().key(format!("l{i}")).absolute(c[0] - 110.0, y).w(220.0).center()
                .child(el().px(4.0).radius(4.0).fill(srgba(0.0, 0.0, 0.0, 0.45)).child(text(name).font(st).color(color))),
        );
    }
    out
}

fn tooltip(s: &State) -> Option<El> {
    let i = s.hover?;
    let q = s.pointer?;
    let m = s.map();
    let it = m.items.get(i)?;
    let g = m.to_game(it.at);
    let (title, kind) = match it.layer {
        Layer::Item => (pretty(&it.f.class), m.categories[it.cat].name.clone()),
        Layer::HomeCave => (it.f.name.clone(), if it.underwater { "Home cave (underwater)".into() } else { "Home cave".into() }),
        Layer::WayStone => (it.f.name.clone(), if it.underwater { "Waystone (underwater)".into() } else { "Waystone".into() }),
        _ => (it.f.name.clone(), String::new()),
    };
    let mut lines = col().gap(2.0).child(text(title).font(style(13.0, 650.0)).color(INK));
    let mut sub = kind;
    if !sub.is_empty() { sub.push_str("  ·  "); }
    sub.push_str(&format!("{:.0}, {:.0}", g[0], g[1]));
    lines = lines.child(text(sub).font(style(11.0, 500.0)).color(INK_DIM));
    let note = it.f.note.trim_start_matches("up");
    if !note.is_empty() { lines = lines.child(text(format!("seen {note}")).font(style(11.0, 500.0)).color(INK_DIM)); }
    let x = (q[0] + 16.0).min(s.vp[0] - 240.0);
    Some(el().key("tip").absolute(x, q[1] + 14.0).max_w(260.0).padding(8.0).radius(6.0).fill(PANEL).border(1.0, PANEL_EDGE).child(lines))
}

// ------------------------------------------------------------------ chrome

fn button(key: String, label: &str, active: bool, f: impl Fn(&mut State) + 'static) -> El {
    el().key(key).px(10.0).h(28.0).center().radius(6.0)
        .fill(if active { srgba(0.91, 0.69, 0.29, 0.22) } else { srgba(1.0, 1.0, 1.0, 0.06) })
        .border(1.0, if active { ACCENT.alpha(0.6) } else { PANEL_EDGE })
        .on_press(f)
        .child(text(label.to_string()).font(style(12.5, 600.0)).color(if active { ACCENT } else { INK }))
}

fn top_bar(s: &State) -> El {
    let left = if s.panel { 272.0 } else { 12.0 };
    let mut bar = row().key("top").absolute(left, 12.0).gap(6.0).padding(6.0).radius(8.0).fill(PANEL).border(1.0, PANEL_EDGE)
        .child(button("panel".into(), if s.panel { "◀" } else { "☰" }, false, |s: &mut State| s.panel = !s.panel));
    for (i, m) in s.maps.iter().enumerate() {
        bar = bar.child(button(format!("map{i}"), &m.id, i == s.cur, move |s: &mut State| s.select_map(i)));
    }
    bar = bar
        .child(button("zin".into(), "+", false, |s: &mut State| { let q = [s.vp[0] / 2.0, s.vp[1] / 2.0]; s.zoom_about(q, 1.5) }))
        .child(button("zout".into(), "−", false, |s: &mut State| { let q = [s.vp[0] / 2.0, s.vp[1] / 2.0]; s.zoom_about(q, 1.0 / 1.5) }))
        .child(button("fit".into(), "Fit", false, |s: &mut State| { let f = s.fit(); s.set_cam(f) }));
    if s.locate.is_some() {
        bar = bar.child(button("locate".into(), "Locate", false, |s: &mut State| { if let Some(f) = &s.locate { f() } }));
    }
    if s.player.is_some() {
        bar = bar.child(button("me".into(), "Me", false, |s: &mut State| s.focus_player()));
    }
    if s.hide.is_some() {
        bar = bar.child(button("hide".into(), "✕", false, |s: &mut State| { if let Some(f) = &s.hide { f() } }));
    }
    bar
}

fn check(key: String, label: String, count: Option<usize>, state: Option<bool>, swatch: Option<Color>, indent: f32, f: impl Fn(&mut State) + 'static) -> El {
    let on = state.unwrap_or(false);
    let partial = state.is_none();
    let bx = el().size(14.0, 14.0).radius(3.0)
        .fill(if on { ACCENT } else if partial { ACCENT.alpha(0.45) } else { srgba(1.0, 1.0, 1.0, 0.06) })
        .border(1.0, if on || partial { ACCENT } else { srgba(1.0, 1.0, 1.0, 0.25) });
    let mut r = row().key(key).w_full().h(26.0).padding(0.0).px(8.0).gap(8.0).radius(5.0)
        .align(rune::Align::Center).hover(HOVER).on_press(f)
        .child(el().w(indent))
        .child(bx);
    if let Some(c) = swatch { r = r.child(el().size(9.0, 9.0).radius(4.5).fill(c)); }
    r = r.child(text(label).font(style(12.5, 500.0)).color(if on || partial { INK } else { INK_DIM }).single_line());
    if let Some(n) = count { r = r.child(el().grow(1.0)).child(text(n.to_string()).font(style(11.0, 500.0)).color(INK_DIM)); }
    r
}

fn side_panel(s: &State) -> El {
    let m = s.map();
    let mut list = col().key("layers").w_full().gap(1.0);
    list = list.child(text("LAYERS").font(style(10.5, 700.0)).color(INK_DIM));
    for (layer, name) in LAYERS {
        let n = m.items.iter().filter(|it| it.layer == layer && it.f.kind != "txt").count()
            + m.items.iter().filter(|it| it.layer == layer && it.f.kind == "txt").count();
        if n == 0 { continue; }
        list = list.child(check(format!("ly{name}"), name.to_string(), None, Some(s.layers.contains(&layer)), None, 0.0, move |s: &mut State| {
            if !s.layers.remove(&layer) { s.layers.insert(layer); }
        }));
    }
    list = list.child(el().h(10.0)).child(text("FOOD & ITEMS").font(style(10.5, 700.0)).color(INK_DIM));
    for (ci, cat) in m.categories.iter().enumerate() {
        let on = cat.types.iter().filter(|t| s.items.contains(&t.0)).count();
        let state = if on == 0 { Some(false) } else if on == cat.types.len() { Some(true) } else { None };
        let total: usize = cat.types.iter().map(|t| t.1).sum();
        let open = s.expanded.contains(&ci);
        let header = row().w_full().gap(2.0)
            .child(el().key(format!("ex{ci}")).size(22.0, 26.0).center().radius(5.0).hover(HOVER)
                .on_press(move |s: &mut State| { if !s.expanded.remove(&ci) { s.expanded.insert(ci); } })
                .child(text(if open { "▾" } else { "▸" }).font(style(12.0, 600.0)).color(INK_DIM)))
            .child(el().grow(1.0).child(check(format!("cat{ci}"), cat.name.clone(), Some(total), state, Some(cat_color(ci)), 0.0, move |s: &mut State| {
                let types: Vec<String> = s.map().categories[ci].types.iter().map(|t| t.0.clone()).collect();
                let all = types.iter().all(|t| s.items.contains(t));
                for t in types { if all { s.items.remove(&t); } else { s.items.insert(t); } }
            })));
        list = list.child(header);
        if open {
            for (ty, n) in &cat.types {
                let ty2 = ty.clone();
                list = list.child(check(format!("ty{ci}_{ty}"), pretty(ty), Some(*n), Some(s.items.contains(ty)), None, 22.0, move |s: &mut State| {
                    if !s.items.remove(&ty2) { s.items.insert(ty2.clone()); }
                }));
            }
        }
    }
    let footer = col().w_full().gap(2.0).padding(10.0)
        .child(text(format!("{} · data {}", m.name, m.updated)).font(style(11.0, 600.0)).color(INK_DIM))
        .child(text(format!("Game build {}", m.version)).font(style(10.5, 500.0)).color(INK_DIM))
        .child(text("Map data & imagery: VulnonaMAP by Coco.N (vulnona.com)").font(style(10.5, 500.0)).color(INK_DIM).wrap());
    col().key("panel").absolute(12.0, 12.0).w(250.0).h(s.vp[1] - 24.0).radius(10.0).fill(PANEL).border(1.0, PANEL_EDGE)
        .child(row().w_full().h(44.0).px(14.0).align(rune::Align::Center)
            .child(text("titanmap").font(style(15.0, 700.0)).color(ACCENT)))
        .child(col().key("panel-scroll").w_full().grow(1.0).min_h(0.0).px(8.0).scroll(ScrollSpec { stick_bottom: false }).child(list))
        .child(footer)
}

fn status_bar(s: &State) -> El {
    let m = s.map();
    let mut parts = Vec::new();
    if let Some(q) = s.pointer {
        let g = m.to_game(s.to_image(s.cam(), q));
        parts.push(format!("{:.0}, {:.0}", g[0], g[1]));
    }
    if !s.status.is_empty() { parts.push(s.status.clone()); }
    if parts.is_empty() { return el().key("status"); }
    el().key("status").pin(None, None, Some(12.0), Some(12.0)).px(10.0).py(5.0).radius(6.0).fill(PANEL).border(1.0, PANEL_EDGE)
        .child(text(parts.join("   ·   ")).font(style(12.0, 500.0)).color(INK))
}
