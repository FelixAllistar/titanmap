//! datagen: pull Path of Titans map data from VulnonaMAP (https://vulnona.com/game/map/)
//! and convert it into what titanmap draws:
//!
//!   assets/maps/<Map>/map.json          bounds, image size, every feature
//!   assets/maps/<Map>/tiles/<z>/<x>_<y>.jpg   a 512 px tile pyramid of the base image
//!
//! Raw downloads are cached in datagen/cache/<Map>/<update-tag>/ so re-running only
//! fetches what changed upstream. Vulnona's author asks for a link back; titanmap credits
//! them in the app and README.
//!
//! cargo run --release -p datagen [-- Gondwa Riparia ...]

mod icons;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use image::{GenericImageView, imageops::FilterType};
use serde::Serialize;

const BASE: &str = "https://vulnona.com/game/map";
const MAPS: &[&str] = &["Gondwa", "Riparia", "Panjura"];
const TILE: u32 = 512;
const UA: &str = "titanmap-datagen/0.1 (+https://github.com/FelixAllistar/titanmap)";

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let cache = root.join("datagen/cache");
    let out = root.join("assets/maps");
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    let skip_tiles = wanted.iter().any(|a| a == "--no-tiles");
    let icons_only = wanted.iter().any(|a| a == "--icons");
    let wanted: Vec<String> = wanted.into_iter().filter(|a| !a.starts_with("--")).collect();
    if icons_only { build_icons(&root, &cache, &out); return; }

    let dat = get_text(&format!("{BASE}/dat.txt"));
    fs::create_dir_all(&cache).unwrap();
    fs::write(cache.join("dat.txt"), &dat).unwrap();
    let meta = parse_dat(&dat);

    let mut index = Vec::new();
    for &id in MAPS {
        let Some(m) = meta.get(id) else { eprintln!("{id}: not in dat.txt, skipped"); continue };
        index.push(serde_json::json!({ "id": id, "name": m.name, "version": m.version, "updated": m.updated }));
        if !wanted.is_empty() && !wanted.iter().any(|w| w == id) { continue; }
        println!("== {id} ({}, data {}, {})", m.name, m.updated, m.version);
        let dir = cache.join(id).join(&m.updated);
        fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        for i in 1..=3 {
            let path = dir.join(format!("data_{i}.txt"));
            let body = cached(&path, &format!("{BASE}/map/{id}/data_{i}.txt"));
            if let Some(b) = body { text.push_str(&String::from_utf8_lossy(&b)); text.push('\n'); }
        }
        let img_path = dir.join("base.webp");
        let img_bytes = cached(&img_path, &format!("{BASE}/map/{id}/base/4.webp?{}", m.updated)).expect("base image");
        let img = image::load_from_memory(&img_bytes).expect("decode base image");
        let (w, h) = img.dimensions();
        println!("   base image {w}x{h}");

        let parsed = parse_data(&text);
        let map_out = out.join(id);
        fs::create_dir_all(&map_out).unwrap();
        let levels = if skip_tiles { level_count(w, h) } else { write_tiles(&img, &map_out.join("tiles")) };
        let doc = MapDoc {
            id: id.into(),
            name: m.name.clone(),
            version: m.version.clone(),
            updated: m.updated.clone(),
            source: format!("{BASE}/#map={id}"),
            bounds: parsed.bounds,
            swap_axes: parsed.swap_axes,
            image: ImageInfo { w, h, tile: TILE, levels },
            features: parsed.features,
        };
        let counts = doc.features.iter().fold(BTreeMap::<&str, usize>::new(), |mut c, f| { *c.entry(f.kind).or_default() += 1; c });
        println!("   features: {counts:?}");
        fs::write(map_out.join("map.json"), serde_json::to_string(&doc).unwrap()).unwrap();
    }
    fs::write(out.join("index.json"), serde_json::to_string_pretty(&index).unwrap()).unwrap();
    build_icons(&root, &cache, &out);
}

/// Icons for every point class any map uses.
fn build_icons(root: &Path, cache: &Path, out: &Path) {
    let mut classes = BTreeSet::new();
    for id in MAPS {
        let Ok(doc) = fs::read_to_string(out.join(id).join("map.json")) else { continue };
        let v: serde_json::Value = serde_json::from_str(&doc).unwrap();
        for f in v["features"].as_array().into_iter().flatten() {
            if f["kind"] == "pt" { if let Some(c) = f["class"].as_str() { classes.insert(c.to_string()); } }
        }
    }
    icons::build(root, cache, &classes);
}

// ---------------------------------------------------------------- fetching

fn agent() -> ureq::Agent { ureq::AgentBuilder::new().user_agent(UA).build() }

fn get_text(url: &str) -> String {
    agent().get(url).call().unwrap_or_else(|e| panic!("{url}: {e}")).into_string().unwrap()
}

/// The file at `path`, downloading it first if it isn't cached. None on a 404.
fn cached(path: &Path, url: &str) -> Option<Vec<u8>> {
    if let Ok(b) = fs::read(path) { return Some(b); }
    print!("   GET {url} ... ");
    match agent().get(url).call() {
        Ok(r) => {
            let mut b = Vec::new();
            std::io::Read::read_to_end(&mut r.into_reader(), &mut b).unwrap();
            println!("{} bytes", b.len());
            fs::write(path, &b).unwrap();
            Some(b)
        }
        Err(ureq::Error::Status(404, _)) => { println!("404"); None }
        Err(e) => panic!("{url}: {e}"),
    }
}

// ---------------------------------------------------------------- dat.txt

struct MapMeta { name: String, version: String, updated: String }

fn parse_dat(dat: &str) -> BTreeMap<String, MapMeta> {
    let mut out = BTreeMap::new();
    for line in dat.lines() {
        let f: Vec<&str> = line.split('\t').map(str::trim).filter(|s| !s.is_empty()).collect();
        // map  class  game  id  name  version  lastup  sizes
        if f.first() == Some(&"map") && f.len() >= 7 && f[2] == "PoT" {
            let version = f[5].trim_start_matches(['✅', '⚠', '❌', '\u{fe0f}']).trim().to_string();
            out.insert(f[3].to_string(), MapMeta { name: f[4].into(), version, updated: f[6].into() });
        }
    }
    out
}

// ---------------------------------------------------------------- data_N.txt

#[derive(Serialize)]
struct MapDoc {
    id: String,
    name: String,
    version: String,
    updated: String,
    source: String,
    /// [min_x, min_y, max_x, max_y] in game map units.
    bounds: [f32; 4],
    swap_axes: bool,
    image: ImageInfo,
    features: Vec<Feature>,
}

#[derive(Serialize)]
struct ImageInfo { w: u32, h: u32, tile: u32, levels: u32 }

/// One thing on the map. `kind`: "pt" (icon or food/item), "txt" (label), "line" (polyline or
/// polygon, `paths` split at Vulnona's M marks), "ell" (ellipse: `p` centre, `r` radii, `rot`).
#[derive(Serialize, Default)]
struct Feature {
    kind: &'static str,
    /// Top-level folder (HomeCave, WayStone:ocean, Area, Water, Locations, _Cave_, Foods & Items, ...).
    layer: String,
    /// Second-level folder (Discover (MiniPoI), - Terres -, ...), if any.
    #[serde(skip_serializing_if = "String::is_empty")]
    sub: String,
    /// Vulnona's class: icon type (HomeCave, Rock, Bone, ...) or area/water/land/cave.
    class: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    name: String,
    /// Display options from Vulnona (uw = underwater, small, fill, B1 = cave floor, ...).
    #[serde(skip_serializing_if = "String::is_empty")]
    opt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    p: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    paths: Vec<Vec<[f32; 2]>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    r: Option<[f32; 2]>,
    #[serde(skip_serializing_if = "is_zero")]
    rot: f32,
    /// Free note (Vulnona's "up2024/09/27" dates, "Add at #40989", "exit", ...).
    #[serde(skip_serializing_if = "String::is_empty")]
    note: String,
}

fn is_zero(v: &f32) -> bool { *v == 0.0 }

struct Parsed { bounds: [f32; 4], swap_axes: bool, features: Vec<Feature> }

fn num(s: &str) -> Option<f32> { s.trim().parse().ok() }

fn clean_label(s: &str) -> String {
    let s = s.replace("<br>", "\n").replace("<BR>", "\n");
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() {
        match c { '<' => tag = true, '>' => tag = false, _ if !tag => out.push(c), _ => {} }
    }
    out.trim().to_string()
}

fn parse_data(text: &str) -> Parsed {
    let mut cfg: BTreeMap<String, String> = BTreeMap::new();
    let mut dirs: Vec<String> = Vec::new();
    let mut features = Vec::new();
    let mut header: Option<Vec<String>> = None;
    let mut body: Vec<Vec<String>> = Vec::new();

    let mut flush = |header: &mut Option<Vec<String>>, body: &mut Vec<Vec<String>>, dirs: &Vec<String>, features: &mut Vec<Feature>| {
        if let Some(h) = header.take() {
            if let Some(f) = build(&h, body, dirs) { features.push(f); }
        }
        body.clear();
    };

    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("#cfg") {
            let f: Vec<&str> = rest.split('\t').map(str::trim).filter(|s| !s.is_empty()).collect();
            if f.len() >= 2 { cfg.insert(f[0].into(), f[1].into()); }
            continue;
        }
        if line.starts_with('#') || line.trim().is_empty() {
            flush(&mut header, &mut body, &dirs, &mut features);
            continue;
        }
        let first = line.split('\t').next().unwrap_or("");
        match first {
            "dir" => {
                flush(&mut header, &mut body, &dirs, &mut features);
                dirs.push(line.split('\t').nth(1).unwrap_or("").trim().to_string());
            }
            "dirEnd" => {
                flush(&mut header, &mut body, &dirs, &mut features);
                dirs.pop();
            }
            "icon" | "food" | "text" | "line" | "path" | "circle" | "cliff" | "photo" => {
                flush(&mut header, &mut body, &dirs, &mut features);
                header = Some(line.split('\t').map(|s| s.trim().to_string()).collect());
            }
            _ if header.is_some() => body.push(line.split(',').map(|s| s.trim().to_string()).collect()),
            _ => {}
        }
    }
    flush(&mut header, &mut body, &dirs, &mut features);

    let g = |k: &str, d: f32| cfg.get(k).and_then(|v| num(v)).unwrap_or(d);
    Parsed {
        bounds: [g("min_X", -400.0), g("min_Y", -400.0), g("max_X", 400.0), g("max_Y", 400.0)],
        swap_axes: cfg.get("axis_X").map(|v| v == "H").unwrap_or(false),
        features,
    }
}

fn build(h: &[String], body: &[Vec<String>], dirs: &[String]) -> Option<Feature> {
    let at = |i: usize| h.get(i).cloned().unwrap_or_default();
    let mut f = Feature {
        layer: dirs.first().cloned().unwrap_or_default(),
        sub: dirs.get(1).cloned().unwrap_or_default(),
        class: at(1),
        name: at(2),
        opt: at(3),
        ..Default::default()
    };
    let first = body.first()?;
    let xy = |r: &Vec<String>| Some([num(r.first()?)?, num(r.get(1)?)?]);
    match h[0].as_str() {
        "icon" | "food" => {
            f.kind = "pt";
            f.p = Some(xy(first)?);
            f.note = first.get(2).cloned().unwrap_or_default();
            // Food can carry an area "rx/ry/rot" as its fourth field.
            if let Some(area) = first.get(3).filter(|s| !s.is_empty()) {
                let v: Vec<f32> = area.split('/').filter_map(num).collect();
                if v.len() >= 2 { f.r = Some([v[0], v[1]]); f.rot = v.get(2).copied().unwrap_or(0.0); }
            }
        }
        "text" => {
            f.kind = "txt";
            f.p = Some(xy(first)?);
            let label = first[2..].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(",");
            f.name = clean_label(&label);
            if f.name.is_empty() { return None; }
        }
        "line" | "path" | "cliff" => {
            f.kind = "line";
            let mut cur: Vec<[f32; 2]> = Vec::new();
            for r in body {
                let Some(p) = xy(r) else { continue };
                if r.get(2).map(|s| s == "M").unwrap_or(false) && !cur.is_empty() {
                    f.paths.push(std::mem::take(&mut cur));
                }
                if r.get(3).map(|s| s == "exit").unwrap_or(false) { f.note = "exit".into(); }
                cur.push(p);
            }
            if !cur.is_empty() { f.paths.push(cur); }
            if f.paths.is_empty() { return None; }
            if h[0] == "cliff" { f.opt = format!("cliff {}", f.opt).trim().to_string(); }
        }
        "circle" => {
            f.kind = "ell";
            f.p = Some(xy(first)?);
            f.r = Some([num(first.get(2)?)?, num(first.get(3)?)?]);
            f.rot = first.get(4).and_then(|s| num(s)).unwrap_or(0.0);
        }
        _ => return None,
    }
    // Names like "BigQuillLake:pos" / ":txt" are Vulnona's ids; keep the readable part.
    if f.kind != "txt" {
        if let Some((n, _)) = f.name.split_once(':') { f.name = n.to_string(); }
    }
    Some(f)
}

// ---------------------------------------------------------------- tiles

fn level_count(w: u32, h: u32) -> u32 {
    let mut levels = 1;
    let mut s = w.max(h);
    while s > TILE { s = s.div_ceil(2); levels += 1; }
    levels
}

/// Level `levels - 1` is full resolution; level 0 fits in one tile.
fn write_tiles(img: &image::DynamicImage, dir: &Path) -> u32 {
    let (w, h) = img.dimensions();
    let levels = level_count(w, h);
    let _ = fs::remove_dir_all(dir);
    let mut cur = img.to_rgb8();
    for z in (0..levels).rev() {
        let (lw, lh) = cur.dimensions();
        let zdir = dir.join(z.to_string());
        fs::create_dir_all(&zdir).unwrap();
        let (nx, ny) = (lw.div_ceil(TILE), lh.div_ceil(TILE));
        for ty in 0..ny {
            for tx in 0..nx {
                let (x0, y0) = (tx * TILE, ty * TILE);
                let tile = image::imageops::crop_imm(&cur, x0, y0, TILE.min(lw - x0), TILE.min(lh - y0)).to_image();
                let mut bytes = Vec::new();
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 82).encode_image(&tile).unwrap();
                fs::write(zdir.join(format!("{tx}_{ty}.jpg")), bytes).unwrap();
            }
        }
        println!("   level {z}: {lw}x{lh}, {}x{} tiles", nx, ny);
        if z > 0 { cur = image::imageops::resize(&cur, lw.div_ceil(2), lh.div_ceil(2), FilterType::Triangle); }
    }
    levels
}
