#!/usr/bin/env python3
"""Generates the app icon sources: a light and a dark version.

  icon.svg        light, the default everywhere (Windows .exe, Linux, macOS before 26)
  icon-dark.svg   dark, the same icon on a charcoal tile (for previews and docs)
  AppIcon.icon/   Icon Composer document for macOS 26+, which shows the light or
                  dark version (and the clear and tinted ones) as the system asks

Regenerate the icons from the repo root with:
  python3 src-tauri/icons/make_icon.py src-tauri/icons
  rsvg-convert -w 1024 -h 1024 src-tauri/icons/icon.svg -o src-tauri/icons/app-icon.png
  cargo tauri icon src-tauri/icons/app-icon.png -o src-tauri/icons
  rm -rf src-tauri/icons/android src-tauri/icons/ios

Preview the macOS versions with Icon Composer's ictool (Xcode 26+), e.g.:
  "$(xcode-select -p)/../Applications/Icon Composer.app/Contents/Executables/ictool" \\
    src-tauri/icons/AppIcon.icon --export-image --output-file dark.png --platform macOS \\
    --rendition Dark --width 1024 --height 1024 --scale 1

Design: a block of tofu (OpenTofu) drawn as an isometric cube, lit from above,
inside two amber circular arrows: the infrastructure being brought up to date.
Light is a blue block on a white tile, dark a light-blue block on a charcoal
tile (the Deployer / Remoter / Subduer palette).

For Christmas (as in Paddock, Hot Mess and Subduer) the block wears a Santa
hat, snow falls on the tile, and the arrows turn red and green. A later
release takes it out: drop hat(), snow() and the CHRISTMAS colours.
"""
import json, math, os, sys

# Per version: tile gradient (top, bottom), mark colour, colour the faces fade
# towards, amber, and the opacity of the hairline round the tile.
PALETTES = {
    "light": dict(bg=("#ffffff", "#dce5f2"), mark="#1f5bd6", fade="#ffffff", amber="#f29d0c",
                  inner="#ffffff", edge=0.10,
                  # face shading: share of the mark colour in (top, left, right)
                  faces=(0.42, 1.0, 0.72)),
    "dark": dict(bg=("#2b3445", "#0d1119"), mark="#6fa8ff", fade="#1a2130", amber="#ffb21e",
                 inner="#141a25", edge=0.0,
                 faces=(1.0, 0.74, 0.50)),
}

# Christmas: the arrows green then red (in place of amber), the hat's red, and
# the snow's colour and opacity on the tile.
CHRISTMAS = {
    "light": dict(arrows=("#17804a", "#d62b33"), red="#d62b33", snow=("#9fbad6", 0.6)),
    "dark": dict(arrows=("#4cc47e", "#ff4f58"), red="#ff4f58", snow=("#ffffff", 0.5)),
}
for mode, extra in CHRISTMAS.items():
    PALETTES[mode].update(extra)

CX, CY = 512, 516           # centre of the block and of the arrows
SIDE = 196                  # cube edge length (isometric)
RADIUS = 300                # arrows' circle
ARROW_W = 52                # amber stroke, as Deployer's arrow
HEAD = 100                  # arrowhead barb length
# Two clockwise arcs (degrees, 0 = right, increasing clockwise on screen).
ARCS = ((168, 312), (348, 492))


def mix(a, b, t):
    """t of colour a, 1-t of colour b."""
    ca = [int(a[i:i+2], 16) for i in (1, 3, 5)]
    cb = [int(b[i:i+2], 16) for i in (1, 3, 5)]
    return "#" + "".join(f"{round(x*t + y*(1-t)):02x}" for x, y in zip(ca, cb))


def block(p):
    """The back layer: the tofu block, three shaded faces with soft corners."""
    w = SIDE * math.cos(math.radians(30))
    h = SIDE / 2
    T, L, R, C = (CX, CY - SIDE), (CX - w, CY - h), (CX + w, CY - h), (CX, CY)
    BL, BR, B = (CX - w, CY + h), (CX + w, CY + h), (CX, CY + SIDE)
    top_t, left_t, right_t = p["faces"]
    faces = (
        ((T, R, C, L), mix(p["mark"], p["fade"], top_t)),
        ((L, C, B, BL), mix(p["mark"], p["fade"], left_t)),
        ((C, R, BR, B), mix(p["mark"], p["fade"], right_t)),
    )
    out = []
    for pts, color in faces:
        d = "M" + " L".join(f"{x:.1f},{y:.1f}" for x, y in pts) + " Z"
        # Same-colour round-joined stroke softens the corners like the other icons' rx.
        out.append(f'<path d="{d}" fill="{color}" stroke="{color}" stroke-width="34" stroke-linejoin="round"/>')
    return out


def point(deg, r=RADIUS):
    a = math.radians(deg)
    return CX + r * math.cos(a), CY + r * math.sin(a)


def arrows(p):
    """The front layer: two arcs chasing each other round the block (amber, or
    red and green for Christmas)."""
    parts = []
    colors = p.get("arrows", (p["amber"], p["amber"]))
    for (start, end), color in zip(ARCS, colors):
        x0, y0 = point(start)
        x1, y1 = point(end)
        arc = f"M{x0:.1f},{y0:.1f} A{RADIUS},{RADIUS} 0 0 1 {x1:.1f},{y1:.1f}"
        # Arrowhead at the end, pointing along the clockwise tangent.
        a = math.radians(end)
        tx, ty = -math.sin(a), math.cos(a)              # direction of travel
        nx, ny = math.cos(a), math.sin(a)               # outward normal
        back = HEAD * math.cos(math.radians(45))
        spread = HEAD * math.sin(math.radians(45))
        b1 = (x1 - tx*back + nx*spread, y1 - ty*back + ny*spread)
        b2 = (x1 - tx*back - nx*spread, y1 - ty*back - ny*spread)
        head = f"M{b1[0]:.1f},{b1[1]:.1f} L{x1:.1f},{y1:.1f} L{b2[0]:.1f},{b2[1]:.1f}"
        for d, join in ((arc, ""), (head, ' stroke-linejoin="round"')):
            parts.append(f'<path d="{d}" fill="none" stroke="{color}" stroke-width="{ARROW_W}" '
                         f'stroke-linecap="round"{join}/>')
    return parts


def snow(p):
    """Christmas: flakes scattered over the tile, clear of the arrows, the same every time."""
    color, opacity = p["snow"]
    flakes = [(196, 220, 12), (300, 166, 8), (846, 204, 10), (788, 150, 7), (170, 404, 8),
              (872, 372, 9), (152, 610, 11), (880, 640, 8), (214, 826, 9), (826, 840, 11),
              (420, 884, 7), (628, 880, 9), (520, 150, 7), (690, 860, 6), (262, 300, 6)]
    return [f'<g fill="{color}" opacity="{opacity}">'
            + "".join(f'<circle cx="{x}" cy="{y}" r="{r}"/>' for x, y, r in flakes) + "</g>"]


def hat(p):
    """Christmas: a Santa hat on the block's top face, its tip flopping to the right."""
    top = CY - SIDE                 # the block's top corner
    return [f'<g transform="translate({CX - 4},{top + 62}) rotate(-12) scale(0.6)">'
            f'<path d="M-96,-6 C-84,-130 -20,-210 70,-214 C126,-216 168,-186 182,-128 '
            f'C150,-160 108,-168 76,-150 C88,-104 96,-52 96,-6 Z" fill="{p["red"]}"/>'
            '<path d="M-40,-30 C-30,-110 10,-170 64,-188" fill="none" stroke="#000" stroke-opacity="0.12" stroke-width="14" stroke-linecap="round"/>'
            '<rect x="-122" y="-34" width="244" height="64" rx="32" fill="#ffffff"/>'
            '<rect x="-122" y="-34" width="244" height="64" rx="32" fill="none" stroke="#000" stroke-opacity="0.1" stroke-width="5"/>'
            '<circle cx="184" cy="-118" r="40" fill="#ffffff"/>'
            '<circle cx="184" cy="-118" r="40" fill="none" stroke="#000" stroke-opacity="0.1" stroke-width="5"/>'
            "</g>"]


def svg(body, defs=""):
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">\n'
            + (f"  <defs>\n{defs}  </defs>\n" if defs else "")
            + "".join(f"  {line}\n" for line in body) + "</svg>\n")


def flat(p):
    """The whole icon on a rounded tile, with the macOS grid's margin and shadow."""
    top, bottom = p["bg"]
    defs = f'''    <linearGradient id="bg" x1="0" y1="0" x2="0.6" y2="1">
      <stop offset="0" stop-color="{top}"/>
      <stop offset="1" stop-color="{bottom}"/>
    </linearGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.14"/>
      <stop offset="0.5" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
    <filter id="shadow" x="-10%" y="-10%" width="120%" height="125%">
      <feDropShadow dx="0" dy="12" stdDeviation="14" flood-color="#000" flood-opacity="0.28"/>
    </filter>
'''
    tile = 'x="100" y="100" width="824" height="824" rx="185"'
    body = [f'<g filter="url(#shadow)"><rect {tile} fill="url(#bg)"/></g>',
            f'<rect {tile} fill="url(#sheen)"/>']
    # A hairline keeps the white tile from melting into light backgrounds.
    if p["edge"]:
        body.append(f'<rect x="102" y="102" width="820" height="820" rx="183" fill="none" stroke="#0b1f5c" stroke-opacity="{p["edge"]}" stroke-width="4"/>')
    body += snow(p) + block(p) + hat(p) + arrows(p)
    return svg(body, defs)


def srgb(hex_color):
    r, g, b = (int(hex_color[i:i+2], 16)/255 for i in (1, 3, 5))
    return f"srgb:{r:.5f},{g:.5f},{b:.5f},1.00000"


def icon_composer(out):
    """AppIcon.icon: the system draws the tile and the glass; we give it the
    tile colours and two layers per version."""
    assets = os.path.join(out, "AppIcon.icon", "Assets")
    os.makedirs(assets, exist_ok=True)
    for mode, p in PALETTES.items():
        for name, layer in (("snow", snow), ("block", block), ("hat", hat), ("arrows", arrows)):
            with open(os.path.join(assets, f"{name}-{mode}.svg"), "w") as f:
                f.write(svg(layer(p)))
    def gradient(mode):
        return {"linear-gradient": [srgb(c) for c in PALETTES[mode]["bg"]], "orientation": {"start": {"x": 0.5, "y": 0}, "stop": {"x": 0.5, "y": 1}}}
    def group(name):
        return {"layers": [{
                    "name": name,
                    "image-name-specializations": [
                        {"value": f"{name}-light.svg"},
                        {"appearance": "dark", "value": f"{name}-dark.svg"},
                    ],
                    "glass": True,
                }],
                "shadow": {"kind": "neutral", "opacity": 0.5},
                "translucency": {"enabled": True, "value": 0.4}}
    doc = {
        "fill-specializations": [
            {"value": gradient("light")},
            {"appearance": "dark", "value": gradient("dark")},
        ],
        "groups": [group("arrows"), group("hat"), group("block"), group("snow")],   # front first
        "supported-platforms": {"squares": ["macOS"]},
    }
    with open(os.path.join(out, "AppIcon.icon", "icon.json"), "w") as f:
        json.dump(doc, f, indent=2)
        f.write("\n")


out = sys.argv[1]
with open(os.path.join(out, "icon.svg"), "w") as f:
    f.write(flat(PALETTES["light"]))
with open(os.path.join(out, "icon-dark.svg"), "w") as f:
    f.write(flat(PALETTES["dark"]))
icon_composer(out)
