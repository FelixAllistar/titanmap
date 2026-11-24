# titanmap

A fast, lightweight map for **Path of Titans** (Gondwa, Riparia, Panjura). It runs as a
desktop overlay over the game, and the same code runs in the browser. Built with
[Rune](https://github.com/anomalyco/rune).

- One hotkey (default **Alt+M**) pops the map over the game; **Esc** or the hotkey again hides it.
- **Auto-locate:** when you open the in-game map (**M**), titanmap takes one screenshot and finds
  where that view sits on the map, so the overlay opens on you at the same zoom. If no map is on
  screen, it drops the screenshot. It reads only screen pixels and key state; it never touches
  game memory or files.
- Home caves, waystones, areas, water, caves, cliffs and named locations, plus every food and item
  type, each toggleable. Hover a marker for its name and coordinates.
- Drag to pan, wheel to zoom, `1`/`2`/`3` switch maps, `L` toggles the panel, `F` centres on you.

## Run (Windows)

```powershell
cargo run --release
```

The game must be in **borderless windowed** (or windowed) mode for any overlay to show on top.
Settings are written to `titanmap.json` next to the executable on first run:

```json
{ "toggle": "alt+m", "watch_key": "M", "watch_delay_ms": 450, "min_score": 0.45 }
```

Set `watch_key` to `""` to turn off watching the map key.

## Web

```bash
./build-web.sh          # needs wasm-bindgen-cli 0.2.129 and Python 3
python -m http.server 8000 -d web-dist
```

## Map data

Imagery and map data come from [VulnonaMAP](https://vulnona.com/game/map/) by Coco.N, whose
readme notes the map images are composites of in-game screenshots (copyright belongs to the
game's developer, Alderon Games) and the POI data is community knowledge. Thanks for keeping it
current. `datagen` re-downloads it, converts it to `assets/maps/<Map>/map.json`, and cuts a
512 px tile pyramid:

```powershell
cargo run --release -p datagen                 # all maps
cargo run --release -p datagen -- Riparia      # one map
```

Raw downloads are cached in `datagen/cache/<Map>/<date>/`, so re-runs only fetch what changed.

## How locating works

`src/locate.rs` crops the screenshot to the map area and slides it over a small copy of the map
at a sweep of scales with normalised cross-correlation (computed with FFTs). It keeps the best
few matches and refines each on a sharper copy of the map. The player is the in-game marker
(found by its blue-and-white dot), or else the centre of the view.

```powershell
cargo run --release --example locate -- shot.png
```

Path of Titans and its maps are © Alderon Games. This is an unofficial fan tool.
