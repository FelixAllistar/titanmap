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
