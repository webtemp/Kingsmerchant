#!/bin/sh
# Regenerate the tray ARGB32 pixmaps from assets/poe2ddd.svg (the Chaos-orb
# nebula, embedded as a base64 PNG). Run after replacing the SVG. Needs
# rsvg-convert + python3 (Pillow). The .argb files are embedded into the binary
# via include_bytes! in platform-linux/src/tray.rs.
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
svg="$here/../poe2ddd.svg"

python3 - "$svg" "$here" <<'PY'
import subprocess, sys
from PIL import Image
svg, outdir = sys.argv[1], sys.argv[2]
for s in (16, 22, 24, 32, 48, 64):
    png = f"/tmp/poe2ddd-tray-{s}.png"
    subprocess.run(["rsvg-convert", "-w", str(s), "-h", str(s), svg, "-o", png], check=True)
    im = Image.open(png).convert("RGBA")
    out = bytearray()
    for r, g, b, a in im.get_flattened_data():   # ARGB32, network byte order
        out += bytes((a, r, g, b))
    open(f"{outdir}/icon{s}.argb", "wb").write(out)
    print(f"icon{s}.argb  ({len(out)} bytes)")
PY
