#!/usr/bin/env python3
"""icon_theme — recolor a harvested vector icon to a target UI THEME, keeping body vs accent
distinct. THE canonical recolor step; nothing else should hand-edit icon colors.

Why this exists (the bug it kills): kdesign/`_kd` and `colorXStroke` icons paint the monochrome
BODY (document outline, frame) with one theme-following class and the COLOURED ACCENT (a blue "W",
green grid, orange "P", red badge) with a separate named class. A naive recolor that picks ONE
colour per icon and slaps it on every class turns the body into the accent colour — so every icon
WITH an accent ships with e.g. a fully-blue document instead of a grey document + blue badge. (Real
regression: PDF-to-Word/Excel/PPT/Picture-to-PDF all had blue/green/orange/red BODIES.) Monochrome
icons survive that bug (their one colour is grey) which is why it hides until someone eyeballs an
accented icon.

The fix: assign colour by CLASS ROLE, not one-per-icon.
  • BODY   classes -> the theme body colour (light grey on a dark bar): `kd-color-icon-primary`/
    `-secondary`, `color{Black,White}{Stroke,Fill}`, `currentColor`, `var(--kd-color-icon-primary…)`.
  • ACCENT classes -> their named accent (overridable): `kd-color-icon-<colourword>-primary`,
    `color{Blue,Green,Orange,Red,…}{Stroke,Fill}`.
  • stroke-width / unknown rules -> left untouched.
Colours MUST be baked into the file (an icon used via `<img src>` can't inherit page colour, and
qlmanage/Chromium ignore CSS custom properties). Pair this with the COLOUR-AWARE vision gate
(`icon_match.gate_icon`) which re-checks body+accent against fresh native pixels — recolour proposes,
the gate disposes. Pure stdlib regex (the files are flat attribute/style lists).
"""
import re

# WPS / kdesign dark-theme palette. Body is the sampled native dark-mode grey (== raw km_darkmode_*).
# Accents are overridable per call (always prefer values SAMPLED from the native crop).
DARK_BODY = "#c7c7c7"
DARK_ACCENTS = {
    "blue": "#3a82f7", "green": "#21a366", "orange": "#e8730c", "red": "#e0533a",
    "yellow": "#f5a623", "purple": "#9b59b6", "cyan": "#17a2b8", "teal": "#17a2b8",
}

_COLORWORDS = "blue|green|orange|red|yellow|purple|cyan|teal|gray|grey|black|white"
_NEUTRAL = {"black", "white", "gray", "grey"}


def _classify(class_name):
    """('body'|'accent'|None, colourword|None) for a CSS class name."""
    n = class_name.lower()
    m = re.search(r"kd-color-icon-(%s)\b" % _COLORWORDS, n)
    if m:
        return ("body", None) if m.group(1) in _NEUTRAL else ("accent", m.group(1))
    if n in ("kd-color-icon-primary", "kd-color-icon-secondary"):
        return ("body", None)
    m = re.search(r"color(%s)(stroke|fill)" % _COLORWORDS, n)
    if m:
        return ("body", None) if m.group(1) in _NEUTRAL else ("accent", m.group(1))
    return (None, None)


def _sub_colors(decl, color):
    """Replace every colour token (var() fallback, #hex, currentColor) in a rule body with `color`.
    Leaves non-colour props (fill-rule, stroke-width values, opacity) intact."""
    decl = re.sub(r"var\(\s*--[^,)]+,\s*[^)]+\)", color, decl)
    decl = re.sub(r"#[0-9a-fA-F]{6,8}\b|#[0-9a-fA-F]{3,4}\b", color, decl)
    decl = decl.replace("currentColor", color)
    return decl


def recolor_svg(svg, body_hex=DARK_BODY, accents=None):
    """Recolour an icon SVG: BODY classes -> `body_hex`, ACCENT classes -> their named accent
    (from `accents`, falling back to `DARK_ACCENTS`). Handles both the raw `var(--…, fallback)`
    form and an already-hardcoded `#hex` form, in `<style>` rules and inline fill/stroke attrs.

    Returns the recoloured SVG as str. Unknown colour schemes (e.g. `stylebaseStroke*`) are left
    untouched on purpose — re-match such an icon to a `_kd`/`colorXStroke` resource (which this
    understands), or rely on the vision gate to reject it."""
    if isinstance(svg, (bytes, bytearray)):
        svg = bytes(svg).decode("utf-8", "ignore")
    palette = {**DARK_ACCENTS, **(accents or {})}

    def fix_rule(m):
        name, body = m.group(1), m.group(2)
        kind, word = _classify(name)
        if kind == "body":
            return ".%s {%s}" % (name, _sub_colors(body, body_hex))
        if kind == "accent" and palette.get(word):
            return ".%s {%s}" % (name, _sub_colors(body, palette[word]))
        return m.group(0)

    svg = re.sub(r"\.([\w-]+)\s*\{([^}]*)\}", fix_rule, svg)
    # Inline (non-<style>) body references.
    svg = re.sub(r'(fill|stroke)="currentColor"', lambda m: '%s="%s"' % (m.group(1), body_hex), svg)
    svg = re.sub(
        r'(fill|stroke)="var\(\s*--kd-color-icon-(?:primary|secondary)[^"]*\)"',
        lambda m: '%s="%s"' % (m.group(1), body_hex),
        svg,
    )
    return svg


def classify_classes(svg):
    """Diagnostic: {class_name: ('body'|'accent'|None, word)} for every class defined in <style>.
    Lets the gate report which icons use an unknown scheme this recolour can't reason about."""
    if isinstance(svg, (bytes, bytearray)):
        svg = bytes(svg).decode("utf-8", "ignore")
    out = {}
    for name in set(re.findall(r"\.([\w-]+)\s*\{", svg)):
        out[name] = _classify(name)
    return out


if __name__ == "__main__":
    import sys
    data = open(sys.argv[1], "rb").read()
    body = sys.argv[2] if len(sys.argv) > 2 else DARK_BODY
    sys.stdout.write(recolor_svg(data, body))
