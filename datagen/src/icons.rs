//! Marker icons in VulnonaMAP's style. Vulnona draws most markers as Noto Color Emoji tinted with
//! CSS filters (`icon.Fish:before { content:"🐟"; filter:brightness(1.3); }`), plus a few small
//! PNGs of its own. We read those rules from its dat.txt, fetch each emoji as a Noto PNG
//! (Google Fonts' Noto Emoji images; the Noto emoji artwork is Apache-2.0), apply the same filters and rotation, and write
//! assets/icons/<Class>.png plus assets/icons/index.json listing what was made.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use image::{Rgba, RgbaImage, imageops::FilterType};

use crate::{BASE, cached, get_text};

const NOTO: &str = "https://fonts.gstatic.com/s/e/notoemoji/latest";
/// Output size in pixels (drawn at ~22 pt, so this stays sharp on 2x screens).
const SIZE: u32 = 48;

struct Rule {
    class: String,
    emoji: Option<String>,
    png: Option<String>,
    filters: Vec<(String, f32)>,
    rotate: f32,
}

pub fn build(root: &Path, cache: &Path, classes: &BTreeSet<String>) {
    // dat.txt lists `icon <tags> <Class> <emoji> <css>`; the page turns each into a CSS rule.
    let dat = get_text(&format!("{BASE}/dat.txt"));
    let css: String = dat.lines().filter_map(|l| {
        let f: Vec<&str> = l.split('\t').filter(|s| !s.trim().is_empty()).collect();
        (f.first() == Some(&"icon") && f.len() >= 4)
            .then(|| {
                // A PNG-only icon has no emoji column: its css comes straight after the class.
                let (emoji, css) = if f[3].contains(':') { ("\u{3000}", &f[3..]) } else { (f[3].trim(), &f[4..]) };
                format!("icon.{}:before {{ content:\"{emoji}\"; {} }}\n", f[2].trim(), css.join(" "))
            })
    }).collect();
    let rules = parse_rules(&css);
    let out = root.join("assets/icons");
    let raw = cache.join("icons");
    fs::create_dir_all(&out).unwrap();
    fs::create_dir_all(&raw).unwrap();
    let mut made = Vec::new();
    for class in classes {
        let Some(rule) = rules.iter().find(|r| &r.class == class) else { continue };
        let src = if let Some(png) = &rule.png {
            let name = png.rsplit('/').next().unwrap();
            cached(&raw.join(format!("v_{name}")), &format!("{BASE}/icon/{name}"))
        } else if let Some(e) = &rule.emoji {
            noto(&raw, e)
        } else { None };
        let Some(bytes) = src else { println!("   icon {class}: no image"); continue };
        let Ok(img) = image::load_from_memory(&bytes) else { println!("   icon {class}: undecodable"); continue };
        let mut img = img.to_rgba8();
        for (f, v) in &rule.filters { filter(&mut img, f, *v); }
        if rule.rotate != 0.0 { img = rotate(&img, rule.rotate); }
        let img = finish(&img);
        img.save(out.join(format!("{class}.png"))).unwrap();
        made.push(class.clone());
    }
    println!("== icons: {} of {} classes", made.len(), classes.len());
    fs::write(out.join("index.json"), serde_json::to_string(&made).unwrap()).unwrap();
}

/// `icon.Fish:before { content:"🐟"; filter:brightness(1.3); }` rules from the page's inline CSS.
fn parse_rules(page: &str) -> Vec<Rule> {
    let mut rules = Vec::new();
    for chunk in page.split("icon.").skip(1) {
        let Some((class, rest)) = chunk.split_once(":before") else { continue };
        if class.is_empty() || !class.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') { continue; }
        let Some(body) = rest.trim_start().strip_prefix('{').and_then(|b| b.split_once('}')).map(|b| b.0) else { continue };
        let mut rule = Rule { class: class.into(), emoji: None, png: None, filters: Vec::new(), rotate: 0.0 };
        for decl in body.split(';') {
            let Some((k, v)) = decl.split_once(':') else { continue };
            let v = v.trim();
            match k.trim() {
                "content" => {
                    let s = v.trim_matches('"');
                    if !s.trim_matches(['\u{3000}', ' ']).is_empty() { rule.emoji = Some(s.into()); }
                }
                "background" | "background-image" => {
                    if let Some(u) = v.split("url(").nth(1).and_then(|u| u.split(')').next()) { rule.png = Some(u.trim().into()); }
                }
                "filter" => {
                    for f in v.split(')') {
                        let Some((name, arg)) = f.trim().split_once('(') else { continue };
                        let arg = arg.trim();
                        let n = if let Some(d) = arg.strip_suffix("deg") { d.parse().unwrap_or(0.0) }
                            else if let Some(p) = arg.strip_suffix('%') { p.parse::<f32>().unwrap_or(100.0) / 100.0 }
                            else { arg.parse().unwrap_or(1.0) };
                        rule.filters.push((name.trim().into(), n));
                    }
                }
                "rotate" => rule.rotate = v.trim_end_matches("deg").parse().unwrap_or(0.0),
                _ => {}
            }
        }
        if !rules.iter().any(|r: &Rule| r.class == rule.class) { rules.push(rule); }
    }
    rules
}

/// Noto images are named by codepoint, without variation selectors: 🐦‍⬛ -> 1f426_200d_2b1b.
fn noto(raw: &Path, emoji: &str) -> Option<Vec<u8>> {
    let cps: Vec<String> = emoji.chars().filter(|&c| c != '\u{fe0f}').map(|c| format!("{:x}", c as u32)).collect();
    let name = cps.join("_");
    cached(&raw.join(format!("noto_{name}.png")), &format!("{NOTO}/{name}/512.png"))
}

// ---------------------------------------------------------------- CSS filters, in sRGB like browsers

fn matrix(img: &mut RgbaImage, m: [[f32; 3]; 3]) {
    for p in img.pixels_mut() {
        let c = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
        for i in 0..3 {
            let v = m[i][0] * c[0] + m[i][1] * c[1] + m[i][2] * c[2];
            p[i] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
}

fn per_channel(img: &mut RgbaImage, f: impl Fn(f32) -> f32) {
    for p in img.pixels_mut() {
        for i in 0..3 { p[i] = (f(p[i] as f32 / 255.0).clamp(0.0, 1.0) * 255.0).round() as u8; }
    }
}

fn saturate_m(s: f32) -> [[f32; 3]; 3] {
    [[0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
     [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
     [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s]]
}

fn filter(img: &mut RgbaImage, name: &str, v: f32) {
    match name {
        "brightness" => per_channel(img, |c| c * v),
        "contrast" => per_channel(img, |c| (c - 0.5) * v + 0.5),
        "saturate" => matrix(img, saturate_m(v)),
        "grayscale" => matrix(img, saturate_m(1.0 - v.min(1.0))),
        "sepia" => {
            let a = 1.0 - v.min(1.0);
            matrix(img, [[0.393 + 0.607 * a, 0.769 - 0.769 * a, 0.189 - 0.189 * a],
                         [0.349 - 0.349 * a, 0.686 + 0.314 * a, 0.168 - 0.168 * a],
                         [0.272 - 0.272 * a, 0.534 - 0.534 * a, 0.131 + 0.869 * a]]);
        }
        "hue-rotate" => {
            let (s, c) = v.to_radians().sin_cos();
            matrix(img, [[0.213 + c * 0.787 - s * 0.213, 0.715 - c * 0.715 - s * 0.715, 0.072 - c * 0.072 + s * 0.928],
                         [0.213 - c * 0.213 + s * 0.143, 0.715 + c * 0.285 + s * 0.140, 0.072 - c * 0.072 - s * 0.283],
                         [0.213 - c * 0.213 - s * 0.787, 0.715 - c * 0.715 + s * 0.715, 0.072 + c * 0.928 + s * 0.072]]);
        }
        _ => {}
    }
}

/// Rotate clockwise by `deg` about the centre (CSS `rotate`), bilinear, same canvas size.
fn rotate(img: &RgbaImage, deg: f32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let (s, c) = deg.to_radians().sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    RgbaImage::from_fn(w, h, |x, y| {
        let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
        let (sx, sy) = (c * dx + s * dy + cx - 0.5, -s * dx + c * dy + cy - 0.5);
        let (x0, y0) = (sx.floor(), sy.floor());
        let (fx, fy) = (sx - x0, sy - y0);
        let mut acc = [0f32; 4];
        for (ox, oy, wt) in [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)] {
            let (px, py) = (x0 as i64 + ox, y0 as i64 + oy);
            if px < 0 || py < 0 || px >= w as i64 || py >= h as i64 { continue; }
            let p = img.get_pixel(px as u32, py as u32);
            let a = p[3] as f32 * wt;
            for i in 0..3 { acc[i] += p[i] as f32 * a; }
            acc[3] += a;
        }
        if acc[3] <= 0.0 { return Rgba([0, 0, 0, 0]); }
        Rgba([(acc[0] / acc[3]) as u8, (acc[1] / acc[3]) as u8, (acc[2] / acc[3]) as u8, acc[3].min(255.0) as u8])
    })
}

/// Scale to SIZE with a little room, and put a soft dark halo underneath so it reads on any terrain.
fn finish(img: &RgbaImage) -> RgbaImage {
    let inner = SIZE - 6;
    let (w, h) = img.dimensions();
    let k = inner as f32 / w.max(h) as f32;
    let (nw, nh) = (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1));
    let small = image::imageops::resize(img, nw, nh, FilterType::Lanczos3);
    let mut out = RgbaImage::new(SIZE, SIZE);
    let (ox, oy) = ((SIZE - nw) / 2, (SIZE - nh) / 2);
    // Halo: the icon's alpha, spread 2 px, at 55% black.
    let alpha = |x: i64, y: i64| -> f32 {
        let (sx, sy) = (x - ox as i64, y - oy as i64);
        if sx < 0 || sy < 0 || sx >= nw as i64 || sy >= nh as i64 { 0.0 } else { small.get_pixel(sx as u32, sy as u32)[3] as f32 / 255.0 }
    };
    for y in 0..SIZE as i64 {
        for x in 0..SIZE as i64 {
            let mut a: f32 = 0.0;
            for dy in -2..=2i64 { for dx in -2..=2i64 { if dx * dx + dy * dy <= 5 { a = a.max(alpha(x + dx, y + dy)); } } }
            out.put_pixel(x as u32, y as u32, Rgba([10, 12, 16, (a * 0.55 * 255.0) as u8]));
        }
    }
    image::imageops::overlay(&mut out, &small, ox as i64, oy as i64);
    out
}
