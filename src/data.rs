//! Map data produced by `datagen` (assets/maps/<Map>/map.json), embedded at build time.

use serde::Deserialize;

pub const MAP_IDS: [&str; 3] = ["Gondwa", "Riparia", "Panjura"];

fn embedded(id: &str) -> &'static str {
    match id {
        "Gondwa" => include_str!("../assets/maps/Gondwa/map.json"),
        "Riparia" => include_str!("../assets/maps/Riparia/map.json"),
        "Panjura" => include_str!("../assets/maps/Panjura/map.json"),
        _ => unreachable!(),
    }
}

/// Classes with a marker icon at assets/icons/<Class>.png (made by `datagen --icons`).
pub fn icon_classes() -> Vec<String> {
    serde_json::from_str(include_str!("../assets/icons/index.json")).unwrap_or_default()
}

/// A food or fish and what eating it does (assets/effects.json, from the community wiki).
#[derive(Deserialize, Clone)]
pub struct Effect {
    pub name: String,
    pub kind: String,
    pub effect: String,
    #[serde(default)]
    pub r#where: String,
    /// Map marker classes that show where to find it.
    #[serde(default)]
    pub classes: Vec<String>,
}

pub fn effects() -> Vec<Effect> {
    #[derive(Deserialize)]
    struct Doc { entries: Vec<Effect> }
    serde_json::from_str::<Doc>(include_str!("../assets/effects.json")).map(|d| d.entries).unwrap_or_default()
}

#[derive(Deserialize)]
pub struct ImageInfo {
    pub w: u32,
    pub h: u32,
    pub tile: u32,
    pub levels: u32,
}

#[derive(Deserialize)]
pub struct Feature {
    pub kind: String,
    pub layer: String,
    #[serde(default)]
    pub sub: String,
    pub class: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub opt: String,
    pub p: Option<[f32; 2]>,
    #[serde(default)]
    pub paths: Vec<Vec<[f32; 2]>>,
    pub r: Option<[f32; 2]>,
    #[serde(default)]
    pub rot: f32,
    #[serde(default)]
    pub note: String,
}

#[derive(Deserialize)]
struct Doc {
    id: String,
    name: String,
    version: String,
    updated: String,
    bounds: [f32; 4],
    image: ImageInfo,
    features: Vec<Feature>,
}

/// What a feature belongs to in the layer panel.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Layer {
    HomeCave,
    WayStone,
    Areas,
    Water,
    Locations,
    Caves,
    Cliffs,
    /// Foods & items, toggled per type.
    Item,
    Other,
}

pub const LAYERS: [(Layer, &str); 7] = [
    (Layer::HomeCave, "Home caves"),
    (Layer::WayStone, "Waystones"),
    (Layer::Locations, "Locations"),
    (Layer::Areas, "Area names"),
    (Layer::Water, "Water names"),
    (Layer::Caves, "Caves"),
    (Layer::Cliffs, "Cliffs"),
];

pub struct Item {
    pub layer: Layer,
    /// For items: index into `MapData::categories`.
    pub cat: usize,
    /// For items: index into that category's `types`.
    pub ty: usize,
    pub underwater: bool,
    pub f: Feature,
    /// Position in full-resolution image pixels (for points, labels, ellipse centres).
    pub at: [f32; 2],
    /// Polylines in image pixels.
    pub lines: Vec<Vec<[f32; 2]>>,
}

pub struct Category {
    pub name: String,
    pub types: Vec<(String, usize)>,
}

pub struct MapData {
    pub id: String,
    pub name: String,
    pub version: String,
    pub updated: String,
    pub bounds: [f32; 4],
    pub image: ImageInfo,
    pub items: Vec<Item>,
    pub categories: Vec<Category>,
}

fn layer_of(f: &Feature) -> Layer {
    let l = f.layer.to_ascii_lowercase();
    let base = l.split(':').next().unwrap_or("");
    match base {
        "homecave" => Layer::HomeCave,
        "waystone" => Layer::WayStone,
        "area" => Layer::Areas,
        "water" => Layer::Water,
        "locations" => Layer::Locations,
        "_cave_" => Layer::Caves,
        "_cliff_" => Layer::Cliffs,
        "foods & items" => Layer::Item,
        _ => Layer::Other,
    }
}

/// "- Fungis, Mushrooms -" -> "Fungi & mushrooms": one name per category across maps.
pub fn category_name(sub: &str) -> String {
    let s = sub.trim().trim_matches('-').trim();
    match s {
        "Fungis, Mushrooms" | "Fungis & Mushrooms" => "Fungi & mushrooms".into(),
        "Other aquas" | "Others aquas" => "Other aquatic".into(),
        "Aquas" => "Aquatic".into(),
        "Terres" => "Terrestrial".into(),
        "Quest Objects" | "Quest Object" => "Quest objects".into(),
        "Quest BlueNPC" => "Quest NPCs".into(),
        "Flower & Weeds" => "Flowers & weeds".into(),
        "" => "Other".into(),
        s => s.to_string(),
    }
}

/// "AI_Burrow" -> "AI Burrow", "RedGinger" -> "Red Ginger".
pub fn pretty(class: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in class.chars() {
        if c == '_' { out.push(' '); prev_lower = false; continue; }
        if c.is_uppercase() && prev_lower { out.push(' '); }
        prev_lower = c.is_lowercase();
        out.push(c);
    }
    out
}

impl MapData {
    pub fn load(id: &str) -> MapData {
        let doc: Doc = serde_json::from_str(embedded(id)).expect("map.json");
        let [x0, y0, x1, y1] = doc.bounds;
        let (w, h) = (doc.image.w as f32, doc.image.h as f32);
        let px = |p: [f32; 2]| [(p[0] - x0) / (x1 - x0) * w, (p[1] - y0) / (y1 - y0) * h];
        let mut categories: Vec<Category> = Vec::new();
        let mut items = Vec::with_capacity(doc.features.len());
        for f in doc.features {
            let layer = layer_of(&f);
            if layer == Layer::Other || f.layer.contains("for dev") || f.sub.contains("for dev") { continue; }
            let (mut cat, mut ty) = (0, 0);
            if layer == Layer::Item {
                let cname = category_name(&f.sub);
                cat = categories.iter().position(|c| c.name == cname).unwrap_or_else(|| {
                    categories.push(Category { name: cname, types: Vec::new() });
                    categories.len() - 1
                });
                let types = &mut categories[cat].types;
                ty = types.iter().position(|t| t.0 == f.class).unwrap_or_else(|| {
                    types.push((f.class.clone(), 0));
                    types.len() - 1
                });
                types[ty].1 += 1;
            }
            let underwater = f.layer.contains(':') || f.opt.split_whitespace().any(|o| o.starts_with("uw"));
            let at = f.p.map(px).unwrap_or([0.0, 0.0]);
            let lines = f.paths.iter().map(|p| p.iter().map(|&q| px(q)).collect()).collect();
            items.push(Item { layer, cat, ty, underwater, f, at, lines });
        }
        for c in &mut categories { c.types.sort_by(|a, b| a.0.cmp(&b.0)); }
        // Re-resolve type indices after sorting.
        for it in &mut items {
            if it.layer == Layer::Item {
                it.ty = categories[it.cat].types.iter().position(|t| t.0 == it.f.class).unwrap();
            }
        }
        MapData {
            id: doc.id,
            name: doc.name,
            version: doc.version,
            updated: doc.updated,
            bounds: doc.bounds,
            image: doc.image,
            items,
            categories,
        }
    }

    /// Image pixels per game unit.
    pub fn px_per_unit(&self) -> f32 { self.image.w as f32 / (self.bounds[2] - self.bounds[0]) }

    pub fn to_px(&self, g: [f32; 2]) -> [f32; 2] {
        let [x0, y0, x1, y1] = self.bounds;
        [(g[0] - x0) / (x1 - x0) * self.image.w as f32, (g[1] - y0) / (y1 - y0) * self.image.h as f32]
    }

    pub fn to_game(&self, p: [f32; 2]) -> [f32; 2] {
        let [x0, y0, x1, y1] = self.bounds;
        [x0 + p[0] / self.image.w as f32 * (x1 - x0), y0 + p[1] / self.image.h as f32 * (y1 - y0)]
    }
}
