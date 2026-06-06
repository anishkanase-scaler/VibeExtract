#!/usr/bin/env python3
"""icon_match — pick each control's icon by VISUAL match to the native pixels, not by name.

Why this exists: name-driven icon selection (keyword / id-map) is two-level-ambiguous and
regresses on every re-pick. Here the native icon PIXELS decide; the name map is only a prior
that shortlists candidates. Confirmed picks are LOCKED in icon_map.json so re-runs never
re-pick them (kills the "fixed one, broke another" regression).

Pipeline:  extract_pool (resource_extract) -> render_masks (cached, colour-invariant)
           -> per control: match(native_crop, masks, name_prior) -> ranked candidates
           -> model confirms once -> lock in icon_map.json.

Color-invariance trick: a rendered candidate's ALPHA channel IS its silhouette regardless of
the glyph's colour; the native crop uses luma + polarity. Both reduce to a binary shape mask,
so theme recolour, background, padding, 16-vs-24px and AA all drop out. Pure PIL (no numpy).
"""
import os, struct, zlib, glob, json, hashlib, subprocess, tempfile, re
from PIL import Image, ImageChops, ImageStat, ImageFilter, ImageOps

try:
    import resource_extract as rx
except ImportError:
    rx = None


# ---------------------------------------------------------------- masks
def _binarize(ink):
    """L-mode single channel -> binary (0/255) via a two-mode histogram split."""
    h = ink.histogram()
    total = sum(h) or 1
    # midpoint between the darkest and brightest populated buckets, weighted to ink
    lo = next((i for i, c in enumerate(h) if c), 0)
    hi = next((i for i in range(255, -1, -1) if h[i]), 255)
    thr = (lo + hi) // 2 if hi > lo else 128
    return ink.point(lambda v: 255 if v > thr else 0)


def _native_ink(crop):
    """Native RGB crop -> binary glyph mask (polarity-aware: glyph may be light-on-dark)."""
    g = crop.convert("L")
    st = ImageStat.Stat(g)
    if st.mean[0] < 128:          # mostly dark bg -> glyph is the bright set (keep)
        ink = g
    else:                          # light bg -> invert so glyph is bright
        ink = ImageOps.invert(g)
    return _binarize(ink)


def _io(b):
    import io
    return io.BytesIO(b)


def _ink_from_rgba(im):
    """RGBA render -> binary silhouette. Use ALPHA only when it carries REAL transparency
    (transparent bg + opaque glyph). qlmanage thumbnails onto an OPAQUE WHITE background, so
    alpha is uniformly 255 (useless) — then use luma instead (dark glyph on light bg → invert)."""
    a = im.split()[3]
    ast = ImageStat.Stat(a)
    if a.getbbox() is not None and ast.mean[0] < 250 and ast.stddev[0] > 8:
        ink = a                                   # meaningful alpha channel
    else:
        ink = ImageOps.invert(im.convert("L"))    # opaque render: glyph is the dark set
    return _binarize(ink)


def normalize(binary, size=64, blur=1.2):
    """binary glyph -> tight-crop -> resize size×size -> soft mask (alignment/AA tolerant)."""
    bb = binary.getbbox()
    if bb:
        binary = binary.crop(bb)
    m = binary.resize((size, size), Image.BILINEAR)
    return m.filter(ImageFilter.GaussianBlur(blur)) if blur else m


def score(a, b):
    """Shape similarity in [0,1] of two normalized soft masks (pure PIL). 0.6*softIoU + 0.4*NCC."""
    inter = ImageStat.Stat(ImageChops.darker(a, b)).sum[0]
    union = ImageStat.Stat(ImageChops.lighter(a, b)).sum[0] or 1
    iou = inter / union
    sa, sb = ImageStat.Stat(a), ImageStat.Stat(b)
    ma, mb = sa.mean[0], sb.mean[0]
    eab = ImageStat.Stat(ImageChops.multiply(a, b)).mean[0] * 255.0    # multiply scales /255
    cov = eab - ma * mb
    denom = (sa.stddev[0] * sb.stddev[0]) or 1e-6
    ncc = max(0.0, min(1.0, cov / denom))
    return 0.6 * iou + 0.4 * ncc


# ---------------------------------------------------------------- candidate rendering (cached)
def render_masks(pool, keys, size=64, cache_dir="cache/masks", blur=1.2, batch=180):
    """{key: normalized soft mask} for the requested keys, content-hash cached as PNG.
    SVGs are rasterised with BATCHED qlmanage calls (one process per ~180 files, not per
    file) — the difference between seconds and minutes for a few-hundred-candidate pool."""
    os.makedirs(cache_dir, exist_ok=True)
    out, todo = {}, []
    for k in keys:
        if k not in pool:
            continue
        h = hashlib.sha1(pool[k]).hexdigest()[:16]
        cp = os.path.join(cache_dir, f"{h}@{size}.png")
        if os.path.exists(cp):
            out[k] = Image.open(cp).convert("L")
        else:
            todo.append((k, h, cp))
    if not todo:
        return out
    with tempfile.TemporaryDirectory() as td:
        svg_jobs = []                                    # (key,h,cp, svg_path)
        for k, h, cp in todo:
            b = pool[k]
            if b[:4] == b"\x89PNG":                      # PNG candidate -> mask directly
                m = normalize(_ink_from_rgba(Image.open(_io(b)).convert("RGBA")), size, blur)
                m.save(cp); out[k] = m
            else:
                sp = os.path.join(td, h + ".svg")
                open(sp, "wb").write(b)
                svg_jobs.append((k, h, cp, sp))
        for i in range(0, len(svg_jobs), batch):          # batched qlmanage
            chunk = svg_jobs[i:i + batch]
            subprocess.run(["qlmanage", "-t", "-s", str(size * 2)] + [j[3] for j in chunk]
                           + ["-o", td], capture_output=True)
        for k, h, cp, sp in svg_jobs:
            pngs = glob.glob(os.path.join(td, h + ".svg*.png")) + glob.glob(os.path.join(td, h + ".png"))
            if not pngs:
                continue
            m = normalize(_ink_from_rgba(Image.open(pngs[0]).convert("RGBA")), size, blur)
            m.save(cp); out[k] = m
    return out


# ---------------------------------------------------------------- matching
def _shortlist(pool_idx, name_prior, keywords, kw_cap=44):
    """candidate keys = prior basename + its size/state variants (ALWAYS) + a CAPPED set of
    keyword neighbours. Uncapped substring keywords explode (e.g. "line"→1000+); we keep the
    prior's own variants unconditionally and add only the `kw_cap` shortest keyword-matching
    basenames (shortest ≈ the base command, not a long derived name)."""
    cands = set()
    priors = [name_prior] if isinstance(name_prior, str) else list(name_prior or [])
    for p in priors:
        for variant in (p, *p.split(";")):                 # state lists "x;x-hover;x-down"
            base = variant.strip()
            for bn, keys in pool_idx.items():
                if bn == base or bn.startswith(base + "_") or base.startswith(bn):
                    cands.update(keys)
    kwl = [k.lower() for k in (keywords or [])]
    hits = [bn for bn in pool_idx if any(k in bn.lower() for k in kwl)]
    for bn in sorted(set(hits), key=lambda b: (len(b), b))[:kw_cap]:
        cands.update(pool_idx[bn])
    return cands


def match(native_crop, pool, pool_idx, name_prior=None, keywords=None,
          size=64, topk=6, prior_boost=0.10, cache_dir="cache/masks"):
    """Rank candidates for ONE control. Returns [(key, score, from_prior)] sorted best-first.
    name_prior only BIASES (prior_boost); a clearly-better visual score still wins."""
    nat = normalize(_native_ink(native_crop), size)
    cand_keys = _shortlist(pool_idx, name_prior, keywords)
    if not cand_keys:                                       # no prior -> broad keyword pool
        cand_keys = set(k for bn, ks in pool_idx.items() for k in ks
                        if any(w in bn.lower() for w in (keywords or [])))
    masks = render_masks(pool, cand_keys, size, cache_dir)
    prior_bases = set()
    for p in ([name_prior] if isinstance(name_prior, str) else (name_prior or [])):
        prior_bases.update(x.strip() for x in p.split(";"))
    ranked = []
    for k, m in masks.items():
        s = score(nat, m)
        is_prior = rx.basename(k) in prior_bases if rx else False
        ranked.append((k, s, s + (prior_boost if is_prior else 0.0), is_prior))
    ranked.sort(key=lambda r: -r[2])                        # sort by biased score
    return [(k, round(s, 3), p) for (k, s, _b, p) in ranked[:topk]]


# ---------------------------------------------------------------- lock (icon_map.json)
def load_map(path):
    return json.load(open(path)) if os.path.exists(path) else {}


def save_map(path, m):
    json.dump(m, open(path, "w"), indent=2, sort_keys=True)


def crop_hash(crop_bytes):
    return hashlib.sha1(crop_bytes).hexdigest()[:16]


def is_locked(entry, ch):
    return bool(entry) and entry.get("confirmed") and entry.get("crop_hash") == ch
