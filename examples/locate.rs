//! Locate screenshots offline: `cargo run --release --example locate -- shot1.png shot2.webp ...`
//! Prints the map, position (game units) and score, and writes `<shot>.fix.png` showing the
//! matched part of our map beside the screenshot.

use std::path::Path;

use titanmap_core::data::{MAP_IDS, MapData};
use titanmap_core::locate::Locator;

fn main() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");
    let maps: Vec<MapData> = MAP_IDS.iter().map(|id| MapData::load(id)).collect();
    let t = std::time::Instant::now();
    let loc = Locator::new(&assets, &maps.iter().map(|m| (m.id.clone(), (m.image.w, m.image.h), m.image.levels)).collect::<Vec<_>>());
    println!("index built in {} ms", t.elapsed().as_millis());
    for arg in std::env::args().skip(1) {
        let shot = image::open(&arg).expect("open screenshot").to_rgba8();
        let mut variants = vec![(arg.clone(), shot.clone())];
        if std::env::var_os("VARIANTS").is_some() {
            let (w, h) = shot.dimensions();
            let big = image::imageops::resize(&shot, w * 9 / 5, h * 9 / 5, image::imageops::FilterType::Triangle);
            variants.push((format!("{arg} x1.8"), big));
            let mut half = image::imageops::crop_imm(&shot, 0, 0, w * 3 / 5, h).to_image();
            variants.push((format!("{arg} left 60%"), half.clone()));
            for p in half.pixels_mut() { for c in 0..3 { p[c] = (p[c] as f32 * 0.7) as u8; } }
            variants.push((format!("{arg} left 60% darker"), half));
        }
        for (arg, shot) in variants {
        match loc.locate(&shot, None) {
            Some(fix) => {
                let m = maps.iter().find(|m| m.id == fix.map).unwrap();
                let g = m.to_game(fix.at);
                println!(
                    "{arg}: {} at ({:.0}, {:.0}) score {:.3} zoom {:.3} marker {:?} in {} ms",
                    fix.map, g[0], g[1], fix.score, fix.zoom, fix.marker, fix.millis
                );
            }
            None => println!("{arg}: no match"),
        }
        }
    }
}
