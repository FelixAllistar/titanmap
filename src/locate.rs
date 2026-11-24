//! Find where an in-game map screenshot sits on our map image. Pixels only: no game memory, no
//! game files.
//!
//! The in-game map draws the same top-down imagery our tiles hold, at some unknown zoom and
//! offset. We crop the screenshot to the map area (no bottom bar), then for a range of scales
//! slide it over a small copy of the map with normalised cross-correlation (computed with FFTs,
//! so each scale costs two 2-D FFTs), keep the best few places, and refine each on a sharper
//! level of the map. The player is where the in-game marker is: found by its blue-and-white
//! dot, else the centre of the view (the game's "Focus Player").

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

/// In-place square transpose in 32x32 blocks (the naive loop thrashes the cache at n = 1024).
fn transpose(buf: &mut [Complex32], n: usize) {
    const B: usize = 32;
    for by in (0..n).step_by(B) {
        for bx in (by..n).step_by(B) {
            for y in by..(by + B).min(n) {
                let start = if bx == by { y + 1 } else { bx };
                for x in start..(bx + B).min(n) { buf.swap(y * n + x, x * n + y); }
            }
        }
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
        let coarse_z = 0;
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

    /// Locate a screenshot. Every map gets the coarse sweep (cheap); only the best map's top
    /// candidates are refined. `prefer` wins ties within a small margin.
    pub fn locate(&self, shot: &RgbaImage, prefer: Option<&str>) -> Option<Fix> {
        let t0 = std::time::Instant::now();
        let dbg = std::env::var_os("TITANMAP_DEBUG").is_some();
        let area = map_area(shot);
        let crop = image::imageops::crop_imm(shot, area.0, area.1, area.2, area.3).to_image();
        let src = Source::new(luma(&crop));
        let mut sweeps: Vec<(f32, &MapIndex, Vec<Cand>)> = self.maps.iter().map(|m| {
            let c = coarse(m, &src);
            let bonus = if prefer.map(|p| m.id.eq_ignore_ascii_case(p)).unwrap_or(false) { 0.03 } else { 0.0 };
            (c.first().map(|c| c.0).unwrap_or(-1.0) + bonus, m, c)
        }).collect();
        sweeps.sort_by(|a, b| b.0.total_cmp(&a.0));
        if dbg {
            for (sc, m, _) in &sweeps { eprintln!("  coarse {}: {sc:.3}", m.id); }
            eprintln!("  coarse sweeps: {} ms", t0.elapsed().as_millis());
        }
        let (_, m, cands) = sweeps.into_iter().next()?;
        let mut fix = refine(m, &src, &cands)?;
        if dbg { eprintln!("  refined: {} ms", t0.elapsed().as_millis()); }
        let marker = find_marker(shot, area);
        // Marker -> image px through the fitted transform; else the view's centre.
        let q = marker.unwrap_or([area.0 as f32 + area.2 as f32 / 2.0, area.1 as f32 + area.3 as f32 / 2.0]);
        fix.at = [fix.at[0] + (q[0] - area.0 as f32) / fix.zoom, fix.at[1] + (q[1] - area.1 as f32) / fix.zoom];
        fix.marker = marker;
        fix.millis = t0.elapsed().as_millis();
        Some(fix)
    }
}

/// (score, coarse px per shot px, u, v)
type Cand = (f32, f32, usize, usize);

/// The cropped screenshot with halvings, so each rescale starts from a nearby size.
struct Source {
    levels: Vec<GrayImage>,
}

impl Source {
    fn new(img: GrayImage) -> Source {
        let mut levels = vec![img];
        while levels.last().unwrap().width() > 64 {
            let l = levels.last().unwrap();
            let next = resize(l, l.width() / 2, l.height() / 2);
            levels.push(next);
        }
        Source { levels }
    }

    fn dims(&self) -> (u32, u32) { self.levels[0].dimensions() }

    /// The screenshot scaled by `f`.
    fn scaled(&self, f: f32) -> Gray {
        let (w, h) = self.dims();
        let (tw, th) = ((w as f32 * f).round().max(1.0) as u32, (h as f32 * f).round().max(1.0) as u32);
        let from = self.levels.iter().rev().find(|l| l.width() >= tw).unwrap_or(&self.levels[0]);
        Gray::from_luma(&resize(from, tw, th))
    }
}

/// Run `f` over `items` on every core.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> Vec<R> + Sync) -> Vec<R> {
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    std::thread::scope(|sc| {
        let f = &f;
        let jobs: Vec<_> = (0..threads).map(|k| sc.spawn(move || {
            items.iter().skip(k).step_by(threads).flat_map(f).collect::<Vec<R>>()
        })).collect();
        jobs.into_iter().flat_map(|j| j.join().unwrap()).collect()
    })
}

/// Sweep scales on the coarse map; best candidates first.
fn coarse(m: &MapIndex, src: &Source) -> Vec<Cand> {
    let (cw, ch) = src.dims();
    let base = &m.coarse;
    let fmax = (base.img.w as f32 * 0.98 / cw as f32).min(base.img.h as f32 * 0.98 / ch as f32);
    let fmin = (20.0 / cw.min(ch) as f32).max(fmax / 25.0);
    let mut scales = Vec::new();
    let mut f = fmax;
    while f >= fmin { scales.push(f); f /= 1.09; }
    let mut cands = par_map(&scales, |&f| base.search(&src.scaled(f), 2).into_iter().map(|(s, u, v)| (s, f, u, v)).collect());
    cands.sort_by(|a, b| b.0.total_cmp(&a.0));
    cands.truncate(4);
    cands
}

/// Refine the strongest coarse candidates on the fine level. Returns a Fix whose `at` is the
/// image position of the crop's top-left corner.
fn refine(m: &MapIndex, src: &Source, cands: &[Cand]) -> Option<Fix> {
    let (cw, ch) = src.dims();
    let top = cands.first()?.0;
    let k = m.fine_scale / m.coarse_scale; // fine px per coarse px
    // (window origin, downsample, searchable window, fine px per shot px)
    let wins: Vec<(u32, u32, f32, Searchable, f32)> = cands.iter().filter(|c| c.0 > top - 0.12).take(2).filter_map(|&(_, f, u, v)| {
        let g = f * k;
        let (tw, th) = (cw as f32 * g, ch as f32 * g);
        let margin = (tw.max(th) * 0.2).max(40.0);
        let x0 = (u as f32 * k - margin).max(0.0) as u32;
        let y0 = (v as f32 * k - margin).max(0.0) as u32;
        let x1 = ((u as f32 * k + tw + margin) as u32).min(m.fine.width());
        let y1 = ((v as f32 * k + th + margin) as u32).min(m.fine.height());
        if x1 <= x0 + 16 || y1 <= y0 + 16 { return None; }
        // Keep each FFT at 512: downsample big windows.
        let ds = (500.0 / (x1 - x0).max(y1 - y0) as f32).min(1.0);
        let win = image::imageops::crop_imm(&m.fine, x0, y0, x1 - x0, y1 - y0).to_image();
        let win = if ds < 1.0 { resize(&win, ((x1 - x0) as f32 * ds) as u32, ((y1 - y0) as f32 * ds) as u32) } else { win };
        Some((x0, y0, ds, Searchable::new(Gray::from_luma(&win)), g))
    }).collect();
    let jobs: Vec<(usize, i32)> = (0..wins.len()).flat_map(|w| (-3..=3).map(move |s| (w, s))).collect();
    let results = par_map(&jobs, |&(w, step)| {
        let (x0, y0, ds, sw, g) = &wins[w];
        let gg = g * 1.03f32.powi(step) * ds;
        sw.search(&src.scaled(gg), 1).into_iter()
            .map(|(s, uu, vv)| (s, gg / ds, *x0 as f32 + uu as f32 / ds, *y0 as f32 + vv as f32 / ds))
            .collect()
    });
    let (score, g, x, y) = results.into_iter().max_by(|a, b| a.0.total_cmp(&b.0))?;
    // g = fine px per shot px, so shot px per full-res px = fine_scale / g.
    Some(Fix { map: m.id.clone(), score, at: [x / m.fine_scale, y / m.fine_scale], zoom: m.fine_scale / g, marker: None, millis: 0 })
}
