//! Find where an in-game map screenshot sits on our map image. Pixels only: no game memory, no
//! game files.
//!
//! The in-game map draws the same top-down imagery our tiles hold, at some unknown zoom and
//! offset. We crop the screenshot to the map area (no bottom bar), then for a range of scales
//! slide it over a small copy of the map with normalised cross-correlation (computed with FFTs,
//! so each scale costs two 2-D FFTs), keep the best few places, and refine each on a sharper
//! level of the map. The player is where the in-game marker is: found by its blue-and-white
//! dot, else the centre of the view (the game's "Focus Player").

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use image::{GrayImage, RgbaImage, imageops::FilterType};
use rustfft::{Fft, FftPlanner, num_complex::Complex32};

/// A grayscale float image.
#[derive(Clone)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<f32>,
}

impl Gray {
    pub fn from_luma(img: &GrayImage) -> Gray {
        Gray { w: img.width() as usize, h: img.height() as usize, px: img.as_raw().iter().map(|&v| v as f32 / 255.0).collect() }
    }

    fn crop(&self, x0: usize, y0: usize, w: usize, h: usize) -> Gray {
        let mut px = Vec::with_capacity(w * h);
        for y in y0..y0 + h { px.extend_from_slice(&self.px[y * self.w + x0..y * self.w + x0 + w]); }
        Gray { w, h, px }
    }
}

fn luma(img: &RgbaImage) -> GrayImage { image::DynamicImage::ImageRgba8(img.clone()).to_luma8() }

/// Downscale with an area-ish filter.
fn resize(img: &GrayImage, w: u32, h: u32) -> GrayImage { image::imageops::resize(img, w.max(1), h.max(1), FilterType::Triangle) }

// ------------------------------------------------------------------ FFT correlation

struct Fft2 {
    n: usize,
    fwd: Arc<dyn Fft<f32>>,
    inv: Arc<dyn Fft<f32>>,
}

impl Fft2 {
    fn new(n: usize) -> Fft2 {
        let mut p = FftPlanner::new();
        Fft2 { n, fwd: p.plan_fft_forward(n), inv: p.plan_fft_inverse(n) }
    }

    fn run(&self, buf: &mut [Complex32], inverse: bool) {
        let n = self.n;
        let f = if inverse { &self.inv } else { &self.fwd };
        f.process(buf); // every row
        transpose(buf, n);
        f.process(buf); // every column
        transpose(buf, n);
    }

    fn padded(&self, g: &Gray) -> Vec<Complex32> {
        let n = self.n;
        let mut buf = vec![Complex32::new(0.0, 0.0); n * n];
        for y in 0..g.h.min(n) {
            for x in 0..g.w.min(n) { buf[y * n + x].re = g.px[y * g.w + x]; }
        }
        buf
    }
}

fn transpose(buf: &mut [Complex32], n: usize) {
    for y in 0..n {
        for x in y + 1..n { buf.swap(y * n + x, x * n + y); }
    }
}

/// A map image prepared for searching: its spectrum and running sums for local statistics.
pub struct Searchable {
    pub img: Gray,
    fft: Fft2,
    spectrum: Vec<Complex32>,
    s1: Vec<f64>,
    s2: Vec<f64>,
}

impl Searchable {
    pub fn new(img: Gray) -> Searchable {
        let n = img.w.max(img.h).next_power_of_two();
        let fft = Fft2::new(n);
        let mut spectrum = fft.padded(&img);
        fft.run(&mut spectrum, false);
        let (w, h) = (img.w, img.h);
        let mut s1 = vec![0f64; (w + 1) * (h + 1)];
        let mut s2 = vec![0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let (mut r1, mut r2) = (0f64, 0f64);
            for x in 0..w {
                let v = img.px[y * w + x] as f64;
                r1 += v;
                r2 += v * v;
                s1[(y + 1) * (w + 1) + x + 1] = s1[y * (w + 1) + x + 1] + r1;
                s2[(y + 1) * (w + 1) + x + 1] = s2[y * (w + 1) + x + 1] + r2;
            }
        }
        Searchable { img, fft, spectrum, s1, s2 }
    }

    fn window(&self, s: &[f64], x: usize, y: usize, w: usize, h: usize) -> f64 {
        let stride = self.img.w + 1;
        s[(y + h) * stride + x + w] - s[y * stride + x + w] - s[(y + h) * stride + x] + s[y * stride + x]
    }

    /// Normalised cross-correlation of `t` at every offset; the best `k` peaks, spaced apart.
    pub fn search(&self, t: &Gray, k: usize) -> Vec<(f32, usize, usize)> {
        let (w, h, n) = (self.img.w, self.img.h, self.fft.n);
        if t.w >= w || t.h >= h || t.w < 8 || t.h < 8 { return Vec::new(); }
        let cnt = (t.w * t.h) as f64;
        let mean = t.px.iter().map(|&v| v as f64).sum::<f64>() / cnt;
        let tz = Gray { w: t.w, h: t.h, px: t.px.iter().map(|&v| (v as f64 - mean) as f32).collect() };
        let tnorm = tz.px.iter().map(|&v| (v as f64).powi(2)).sum::<f64>().sqrt();
        if tnorm < 1e-6 { return Vec::new(); }
        let mut buf = self.fft.padded(&tz);
        self.fft.run(&mut buf, false);
        for (b, s) in buf.iter_mut().zip(&self.spectrum) { *b = s * b.conj(); }
        self.fft.run(&mut buf, true);
        let scale = 1.0 / (n * n) as f64;
        let (uw, vh) = (w - t.w + 1, h - t.h + 1);
        let mut scores = vec![-1f32; uw * vh];
        for v in 0..vh {
            for u in 0..uw {
                let num = buf[v * n + u].re as f64 * scale;
                let a = self.window(&self.s1, u, v, t.w, t.h);
                let b = self.window(&self.s2, u, v, t.w, t.h);
                let var = b - a * a / cnt;
                if var > 1e-6 { scores[v * uw + u] = (num / (tnorm * var.sqrt())) as f32; }
            }
        }
        // Peaks: take the best, blank its neighbourhood, repeat.
        let mut out = Vec::new();
        let r = (t.w.min(t.h) / 3).max(4);
        for _ in 0..k {
            let Some((i, &s)) = scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)) else { break };
            if s <= 0.0 { break; }
            let (u, v) = (i % uw, i / uw);
            out.push((s, u, v));
            for y in v.saturating_sub(r)..(v + r).min(vh) {
                for x in u.saturating_sub(r)..(u + r).min(uw) { scores[y * uw + x] = -1.0; }
            }
        }
        out
    }
}

// ------------------------------------------------------------------ map pyramids

/// Stitch one level of a map's tile pyramid back into an image.
pub fn load_level(assets: &Path, map: &str, z: u32) -> Option<GrayImage> {
    let dir = assets.join("maps").join(map).join("tiles").join(z.to_string());
    let mut tiles = Vec::new();
    let (mut w, mut h) = (0u32, 0u32);
    for e in std::fs::read_dir(&dir).ok()? {
        let p = e.ok()?.path();
        let stem = p.file_stem()?.to_str()?.to_string();
        let (x, y) = stem.split_once('_')?;
        let (x, y): (u32, u32) = (x.parse().ok()?, y.parse().ok()?);
        let img = image::open(&p).ok()?.to_luma8();
        w = w.max(x * 512 + img.width());
        h = h.max(y * 512 + img.height());
        tiles.push((x, y, img));
    }
    let mut out = GrayImage::new(w, h);
    for (x, y, img) in tiles { image::imageops::replace(&mut out, &img, (x * 512) as i64, (y * 512) as i64); }
    Some(out)
}

pub struct MapIndex {
    pub id: String,
    /// Full-resolution image size.
    pub full: (u32, u32),
    /// Coarse level (~500 px) for the scale sweep.
    coarse: Searchable,
    coarse_scale: f32,
    /// Sharper level for refining, kept as an image; windows of it are searched on demand.
    fine: GrayImage,
    fine_scale: f32,
}

impl MapIndex {
    pub fn build(assets: &Path, id: &str, full: (u32, u32), levels: u32) -> Option<MapIndex> {
        let top = levels - 1;
        let coarse_z = 1.min(top);
        let fine_z = 3.min(top);
        let coarse = load_level(assets, id, coarse_z)?;
        let fine = load_level(assets, id, fine_z)?;
        Some(MapIndex {
            id: id.to_string(),
            full,
            coarse_scale: coarse.width() as f32 / full.0 as f32,
            coarse: Searchable::new(Gray::from_luma(&coarse)),
            fine_scale: fine.width() as f32 / full.0 as f32,
            fine,
        })
    }
}

// ------------------------------------------------------------------ locating

#[derive(Clone, Debug)]
pub struct Fix {
    pub map: String,
    /// Correlation in [-1, 1]; above ~0.5 is a confident match.
    pub score: f32,
    /// Player position in full-resolution map image pixels.
    pub at: [f32; 2],
    /// Screenshot pixels per full-resolution image pixel (the in-game zoom).
    pub zoom: f32,
    /// Where in the screenshot the player marker was found, if it was.
    pub marker: Option<[f32; 2]>,
    pub millis: u128,
}

/// The part of a screenshot that is map: everything above the in-game bottom bar, trimmed at
/// the edges where overlays sit.
fn map_area(shot: &RgbaImage) -> (u32, u32, u32, u32) {
    let (w, h) = shot.dimensions();
    // The bottom bar (Back / Quests / Focus Player) is a dark strip: find where rows turn mostly dark.
    let mut bottom = h;
    for y in (h / 2..h).rev() {
        let dark = (0..w).step_by(4).filter(|&x| { let p = shot.get_pixel(x, y); (p[0] as u32 + p[1] as u32 + p[2] as u32) < 75 }).count();
        if dark * 4 < (w as usize) * 3 / 4 * 3 / 4 { bottom = y + 1; break; }
    }
    let bottom = bottom.min(h * 92 / 100);
    let (x0, y0) = (w * 2 / 100, h * 2 / 100);
    (x0, y0, w - 2 * x0, bottom.saturating_sub(y0 + h / 100))
}

/// The player marker: a white ring around a saturated blue dot. Returns its centre.
pub fn find_marker(shot: &RgbaImage, area: (u32, u32, u32, u32)) -> Option<[f32; 2]> {
    let (ax, ay, aw, ah) = area;
    let blue = |x: u32, y: u32| {
        let p = shot.get_pixel(x, y);
        let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
        b > 150 && b - r > 90 && b - g > 40
    };
    let white = |x: u32, y: u32| { let p = shot.get_pixel(x, y); p[0] > 200 && p[1] > 200 && p[2] > 200 };
    let mut best: Option<(u32, [f32; 2])> = None;
    let (w, h) = shot.dimensions();
    for y in ay.max(6)..(ay + ah).min(h - 6) {
        for x in ax.max(6)..(ax + aw).min(w - 6) {
            if !blue(x, y) { continue; }
            // Blue core with white within a few pixels on all four sides.
            let ring = (2..7).any(|d| white(x + d, y)) as u32 + (2..7).any(|d| white(x - d, y)) as u32
                + (2..7).any(|d| white(x, y + d)) as u32 + (2..7).any(|d| white(x, y - d)) as u32;
            if ring >= 3 {
                // Score by how much blue is around (the dot's size), prefer compact dots.
                let n = (0..5).flat_map(|dy| (0..5).map(move |dx| (dx, dy))).filter(|&(dx, dy)| blue(x + dx - 2, y + dy - 2)).count() as u32;
                if best.map(|b| n > b.0).unwrap_or(true) { best = Some((n, [x as f32, y as f32])); }
            }
        }
    }
    best.map(|b| b.1)
}

pub struct Locator {
    pub maps: Vec<MapIndex>,
}

impl Locator {
    /// `maps`: (id, full size, levels).
    pub fn new(assets: &Path, maps: &[(String, (u32, u32), u32)]) -> Locator {
        Locator { maps: maps.iter().filter_map(|(id, full, levels)| MapIndex::build(assets, id, *full, *levels)).collect() }
    }

    /// Locate a screenshot. `prefer` names the map to try first; others are only tried if it fails.
    pub fn locate(&self, shot: &RgbaImage, prefer: Option<&str>) -> Option<Fix> {
        let t0 = std::time::Instant::now();
        let area = map_area(shot);
        let crop = image::imageops::crop_imm(shot, area.0, area.1, area.2, area.3).to_image();
        let gray = luma(&crop);
        let mut order: Vec<&MapIndex> = self.maps.iter().collect();
        if let Some(p) = prefer { order.sort_by_key(|m| !m.id.eq_ignore_ascii_case(p)); }
        let mut best: Option<Fix> = None;
        for m in order {
            if let Some(mut fix) = self.locate_on(m, &gray, area) {
                let marker = find_marker(shot, area);
                // Marker -> image px through the fitted transform; else the view's centre.
                let q = marker.unwrap_or([area.0 as f32 + area.2 as f32 / 2.0, area.1 as f32 + area.3 as f32 / 2.0]);
                fix.at = [fix.at[0] + (q[0] - area.0 as f32) / fix.zoom, fix.at[1] + (q[1] - area.1 as f32) / fix.zoom];
                fix.marker = marker;
                let good = fix.score > 0.55;
                if best.as_ref().map(|b| fix.score > b.score).unwrap_or(true) { best = Some(fix); }
                if good { break; }
            }
        }
        best.map(|mut b| { b.millis = t0.elapsed().as_millis(); b })
    }

    /// Returns a Fix whose `at` is the image position of the crop's top-left corner.
    fn locate_on(&self, m: &MapIndex, gray: &GrayImage, _area: (u32, u32, u32, u32)) -> Option<Fix> {
        let (cw, ch) = gray.dimensions();
        let base = &m.coarse;
        // Sweep: f = coarse px per screenshot px. The crop must fit inside the coarse map.
        let fmax = (base.img.w as f32 * 0.98 / cw as f32).min(base.img.h as f32 * 0.98 / ch as f32);
        let fmin = (24.0 / cw.min(ch) as f32).max(fmax / 40.0);
        let mut cands: Vec<(f32, f32, usize, usize)> = Vec::new(); // score, f, u, v
        let mut f = fmax;
        let mut scaled: HashMap<u32, Gray> = HashMap::new();
        while f >= fmin {
            let t = Gray::from_luma(&resize(gray, (cw as f32 * f).round() as u32, (ch as f32 * f).round() as u32));
            for (s, u, v) in base.search(&t, 2) { cands.push((s, f, u, v)); }
            scaled.insert(f.to_bits(), t);
            f /= 1.07;
        }
        cands.sort_by(|a, b| b.0.total_cmp(&a.0));
        cands.truncate(3);

        // Refine each candidate on the fine level: search a window around it at nearby scales.
        let mut best: Option<(f32, f32, f32, f32)> = None; // score, fine px per shot px, x, y (fine px of crop origin)
        for &(_, f, u, v) in &cands {
            let k = m.fine_scale / m.coarse_scale; // fine px per coarse px
            let g = f * k; // fine px per shot px
            let (tw, th) = (cw as f32 * g, ch as f32 * g);
            let margin = (tw.max(th) * 0.25).max(48.0);
            let x0 = (u as f32 * k - margin).max(0.0) as u32;
            let y0 = (v as f32 * k - margin).max(0.0) as u32;
            let x1 = ((u as f32 * k + tw + margin) as u32).min(m.fine.width());
            let y1 = ((v as f32 * k + th + margin) as u32).min(m.fine.height());
            if x1 <= x0 + 16 || y1 <= y0 + 16 { continue; }
            // Keep the FFT small: downsample the window if it is big.
            let ds = (1024.0 / (x1 - x0).max(y1 - y0) as f32).min(1.0);
            let win = image::imageops::crop_imm(&m.fine, x0, y0, x1 - x0, y1 - y0).to_image();
            let win = if ds < 1.0 { resize(&win, ((x1 - x0) as f32 * ds) as u32, ((y1 - y0) as f32 * ds) as u32) } else { win };
            let sw = Searchable::new(Gray::from_luma(&win));
            for step in -3..=3 {
                let gg = g * 1.025f32.powi(step) * ds;
                let t = Gray::from_luma(&resize(gray, (cw as f32 * gg).round() as u32, (ch as f32 * gg).round() as u32));
                for (s, uu, vv) in sw.search(&t, 1) {
                    if best.map(|b| s > b.0).unwrap_or(true) {
                        best = Some((s, gg / ds, x0 as f32 + uu as f32 / ds, y0 as f32 + vv as f32 / ds));
                    }
                }
            }
        }
        let (score, g, x, y) = best?;
        let zoom = g / m.fine_scale; // shot px per full-res px... inverted below
        // g = fine px per shot px; full px per shot px = g / fine_scale.
        let full_per_shot = zoom;
        Some(Fix {
            map: m.id.clone(),
            score,
            at: [x / m.fine_scale, y / m.fine_scale],
            zoom: 1.0 / full_per_shot,
            marker: None,
            millis: 0,
        })
    }
}
