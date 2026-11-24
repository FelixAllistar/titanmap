#!/usr/bin/env bash
# Build titanmap for the web into web/dist: Rune's web-build.sh, then the map tiles beside it.
# Needs: rustup target wasm32-unknown-unknown, wasm-bindgen-cli 0.2.129, Python 3.
set -euo pipefail
cd "$(dirname "$0")"
out=web-dist

# Rune's script lives in the checkout cargo fetched for our pinned rev.
rune_manifest=$(cargo metadata --format-version 1 | python -c '
import json,sys
m=json.load(sys.stdin)
print(next(p["manifest_path"] for p in m["packages"] if p["name"]=="rune"))')
script="$(dirname "$rune_manifest")/../../scripts/web-build.sh"

# The script calls python3; on Windows that is often only `python`.
shim=""
if ! python3 -c "" 2>/dev/null; then
    shim=$(mktemp -d)
    printf '#!/usr/bin/env bash\nexec python "$@"\n' > "$shim/python3"
    chmod +x "$shim/python3"
    export PATH="$shim:$PATH"
fi

mkdir -p "$out"
bash "$script" titanmap --out "$out" "$@"

# Tiles only: map.json is compiled into the wasm.
for d in assets/maps/*/; do
    id=$(basename "$d")
    mkdir -p "$out/assets/maps/$id"
    rm -rf "$out/assets/maps/$id/tiles"
    cp -r "$d/tiles" "$out/assets/maps/$id/tiles"
done
# Marker icons (made by `cargo run -p datagen -- --icons`).
rm -rf "$out/assets/icons"
cp -r assets/icons "$out/assets/icons"
# Tiles and icons rarely change: let browsers and Cloudflare cache them for a day.
printf '/assets/*
  Cache-Control: public, max-age=86400
' > "$out/_headers"
[[ -n $shim ]] && rm -rf "$shim"
echo "built $out — serve it with: python -m http.server 8000 -d $out"
