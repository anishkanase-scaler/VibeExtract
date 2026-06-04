---
name: replicate-ui
description: Automatically replicate a running macOS app's UI as plain HTML+CSS, using VibeExtract's MCP tools (AX tree, self-screenshots, visual diff) plus the Playwright MCP to render and self-verify until the replica visually matches. Use when the user wants to clone/recreate a desktop app screen or window as web UI.
---

# Replicate a desktop UI (perceive → generate → verify loop)

You drive a closed loop: inspect a running macOS app via the **`vibe-extract`** MCP
server, generate plain self-contained **HTML+CSS**, render it with the
**`playwright`** MCP, and visually diff the render against the native original —
iterating until they match.

Both MCP servers must be connected (`claude mcp list`). If `vibe-extract` is
missing, tell the user to open the VibeExtract app and click **Start MCP server**,
then `claude mcp add --transport http vibe-extract <url>`. See
`docs/REPLICATE_UI_PLAYBOOK.md` for full setup.

## The toolkit & where things live (read once)
There is **ONE common toolkit, no per-app code.** It ships *inside this skill* at
`_shared/` (the **Base directory for this skill** shown at the top of this prompt) — so it
travels with the skill on any machine, with or without this repo:
- `role-map.json` + `markup.py` — AX/ARIA role → semantic HTML tag/attrs (`markup.role_el`,
  `markup.el`, `markup.attrs_str`).
- `interactive.css` — the button/input reset + hover/focus/active.
- `sprite.py` (`SpriteSlicer`) + `cache.py` (`SpriteCache`) — crop real pixels into deduped
  icon sprites (for AX / screenshot-only apps); `cache.py` is also the reuse-cache CLI.
- `gridtext.py` (+ `vision_ocr.swift`) — baked grid labels → real text (AX→OCR→repair).

Reference it **by the skill's base path**, e.g. `python3 "<skill-base>/_shared/cache.py" …`;
below, `_shared/…` is shorthand for that. **There are no app-specific scripts to locate** — the
method (harvest → inventory → generate → verify) is identical for *every* app; for a given
extraction you generate the HTML directly, or write one **thin throwaway generator** into the
output dir that imports this toolkit.

**Outputs are scratch.** Everything you produce for an extraction — the replica `index.html`,
harvested `assets/`, screenshots/captures, sprite `cache/`, and any generator you write — lives
in **`.replicate-ui/<app>/`** in the current working directory. That folder is gitignored and
fully regenerable: **deleting it loses nothing but caching speed.** The only thing that must
persist is the skill (with its `_shared/`).

## The golden rule: coordinates & scale
- All AX bounds and screenshot inputs are in **points**, top-left origin.
- Native screenshots come back at **device pixels** (Retina ≈ 2× points). Each
  `screenshot_*` result includes `px_w`, `px_h`, and `scale` (px/point).
- **Render the replica at the component's POINT size** (CSS px = `point_w × point_h`).
  `compare_images` resizes both sides to a common canvas, so you do **not** need to
  match device pixels — just render at point size and let the diff reconcile scale.

## Loop

1. **Preflight.** Call `check_ax_permission`. If not `trusted`, call
   `request_ax_permission` and tell the user to grant VibeExtract under
   System Settings → Privacy → Accessibility **and** Screen Recording
   (`screencapture` needs the latter or shots come back black). Stop until granted.

2. **Target.** `frontmost_app` (or `list_windows`) to get the target `pid` and the
   window `bounds`. Confirm with the user which window if ambiguous.

2b. **Scope — selection vs whole window.** Call `get_selection`. If `present` is true —
   the user picked component(s) in the desktop app (⌘⇧S on the target app, then click; ⇧+click
   adds more) — and `age_seconds` is recent (under ~600; if older, confirm with the user it's the
   intended pick), switch to **COMPONENT-ONLY mode**: skip the whole-window inventory and replicate
   *just* the selected element(s). For each element, first decide whether to trust its `bounds`:

   **Native reference crop — prefer `crop_path` (healthy picks only).** A *healthy* pick carries
   `crop_path`: a PNG of that element captured AT PICK-TIME (cropped from the owning window, free of
   the pick overlay). It stays valid even if the app has since been **closed or moved to another
   Space**, so use it as the native reference (`compare_images` `a_path`) instead of a live
   `screenshot_region` whenever it's non-null — no need to keep the target app on screen. Fall back to
   `screenshot_region { x,y,w,h }=bounds` only when `crop_path` is null AND the app is still visible.
   For **shallow picks `crop_path` is intentionally null** (the bounds were a click-centered placeholder,
   not the real element) — don't treat that as a capture failure; re-resolve via `extract_component`
   at the click as below (this needs the app still running).

   - **Healthy AX pick** (`ax_shallow` is false **and** role isn't AXMenuBar/AXApplication/AXWindow):
     use its `bounds` (points, top-left) for structure — `ax_subtree_at_point { x: bounds.x+bounds.w/2,
     y: bounds.y+bounds.h/2 }` for roles/structure, the element's `crop_path` (else
     `screenshot_region { x,y,w,h }`=`bounds`) for the native reference, `extract_component { x, y }`
     for a DOM/CSS head-start.
   - **Shallow AX pick** (`ax_shallow` is true, or role ∈ {AXMenuBar, AXApplication, AXWindow}): the
     native AX tree couldn't see the real element — normal for **Electron** apps (Slack, VS Code),
     whose web content isn't exposed to AX. **Do NOT use `bounds`** (it's a click-centred placeholder,
     not the real element). Instead call `extract_component { x: click.x, y: click.y }` (the full
     CDP→AX→screenshot ladder at the exact click). If it returns **ElectronNeedsRelaunch**, tell the
     user and **offer** `relaunch_with_debug_port { bundle_id, display_name, confirm: true }` (this
     quits & reopens that app — only with their OK), then retry `extract_component` at `click`. Use the
     **returned element's** bounds for `screenshot_region` and for sizing the replica.

   Then Generate (6) + verify (7–9) the component against its crop, sized to its point bounds.
   (Multiple selected elements → compose them.) If `present` is false/empty, proceed with the
   **whole-window** flow below.

2c. **Reuse check — METHOD only, NEVER stale pixels.** 🚫 **THE GOLDEN RULE OF REUSE: the
   CURRENT capture (for a selected component, the pick-time `crop_path`) is the SINGLE source of
   truth for every pixel — colours AND sprites alike. NEVER reuse colours, sprite PNGs,
   `window.png`, or a prior `index.html`'s pixels from an earlier run.** A previous capture can be
   from a different theme/appearance/state — reusing its pixels silently ships the WRONG background
   shade or a stale icon (e.g. a dark Share button, or a `#1b1b1b` fill where the live app is
   `#282828`). The dev must never have to point at a wrong colour or icon. So every run **re-derives
   all pixels from the fresh capture: re-sample EVERY colour and re-slice EVERY sprite from it.**
   Run `python3 "<skill-base>/_shared/cache.py" <app> [win_w win_h]` (app = the working-dir name) to
   see what's cached, but reuse is limited to the **METHOD**, not pixels:
   - ✅ **REUSE — generator script** (`build.py`): the layout structure + per-element coordinate
     map. Re-run it against the FRESH capture so it re-samples colours + re-slices sprites anew.
   - ✅ **REUSE — text fonts** (`assets/fonts/*.woff2`): glyph files don't change page-to-page.
   - ✅ **REUSE — grid-OCR labels** (`capture/headers_cache.json`) ONLY when the capture is
     byte-identical (same mtime) — `gridtext.py` already guards on this.
   - ❌ **DO NOT REUSE — colours / sprite PNGs / `window.png` / prior `index.html` pixels.**
     Re-capture (or use the fresh crop) and regenerate these every run. `cache.py` reports these as
     present for speed context, but they are a *stale-pixel trap* — re-derive, don't copy.
   First page of an app → nothing cached → full cost. Thereafter you save the *method* (no
   re-measuring layout, no re-OCR, no re-harvesting fonts) — but pixels are always fresh.

3. **Inventory.** `ax_tree { pid, window_index: 0 }` → the component tree (roles,
   names, values, per-node `bounds`). This is your structural source of truth — a
   screenshot can't give you roles/labels. For Electron apps the AX tree may be
   shallow; if so, use `extract_component { x, y, pid }` for a high-fidelity
   DOM/CSS head-start (it returns `html`+`toon`), or call
   `relaunch_with_debug_port { bundle_id, display_name, confirm: true }` first
   (this quits & reopens the app — only with the user's OK).

3b. **Harvest real assets — ALWAYS (do this every run).** 🚫 **Real assets are mandatory.
   NEVER hand-draw, approximate, or substitute an icon, image, avatar, or font.** Every one of
   them in the replica MUST come from a harvest (`extract_assets`) or the reuse cache — this is
   not optional and not something to defer or "fix later." Hand-drawn SVGs are the #1 cause of a
   replica that looks wrong; the user should never have to point one out. So:
   - **Run `extract_assets { }` on every run** (it reads the LIVE renderer, so it's never stale).
     Per step 2c, reuse only the cached **fonts** (`assets/fonts/*.woff2`) verbatim; re-derive
     **icons/SVGs/images** from this run's harvest, not a prior run's pixels.
   - If the target is **Electron without a debug port**, *offer* `relaunch_with_debug_port` to
     enable the harvest — do **not** fall back to drawing.
   - For **native (AX) apps** (no CDP), icons come from per-element screenshot **sprites**
     (`screenshot_region` slices / `_shared/cache.py: SpriteCache`) — sliced from real pixels,
     never drawn.
   - **Exact-icon fallback:** if a *specific* icon isn't in the harvest's font/svg/image set, get
     it exactly anyway — **crop its pixel region** from a `screenshot_region` of that element
     (transparent-key the background), **or** read its **icon-font codepoint** and embed the
     harvested woff2. Cropping a real glyph beats drawing one, every time.

   On the harvest itself: `extract_assets { }` pulls assets straight from the live renderer via CDP
   (works even when the app isn't frontmost). It writes files under
   `<out>/assets/{fonts,img}` and returns a manifest:
   - `fonts[]` — every `@font-face` woff2 (icon fonts **and** text fonts, e.g.
     Slack's `Slack-Lato-Quip`), saved locally.
   - `icons[]` — `className → codepoint` for icon-**font** glyphs (apps that use
     an icon font), with each glyph's accessible `label`.
   - `svgIcons[]` — for apps that render icons as **inline SVG** (modern Slack:
     ~50 icons named by `data-qa`, e.g. `home-filled`, `search`, `send-filled`),
     the exact SVG written to `assets/icons/<name>.svg` plus its computed `color`.
   - `images[]` — every visible `<img>`/background (avatars, uploaded files,
     workspace icon) saved to `assets/img/<label>.png`, read in-page when CORS
     allows, else captured pixel-perfect via a `Page.captureScreenshot` clip.
   Map each asset to your replica by its `label`/`name` (icons) and by identity
   (images). Inline SVGs use `fill="currentColor"` — inline them and set the
   wrapper `color` to the captured value; size them with your container's
   `svg{width:Npx}` rule (do **not** force `width:100%`, which blows up unsized
   buttons). Keep text on a calibrated stack (the local text font is identical to
   the system one but can re-drift hand-tuned spacing — verify after swapping).

4. **Reference shots.** `screenshot_window { pid }` for the whole window, and
   `screenshot_region { x,y,w,h }` per component (use each node's `bounds`). Each
   returns an inline image (look at it) **and** a saved `path` (use that for diffing).
   Note the `scale`.

5. **Decompose.** Split the window into its top-level AX regions (title bar,
   sidebar, toolbar, content). Replicate and verify each **independently** against
   its native crop, then compose into one document and verify the whole window.

6. **Generate.** Write plain, self-contained **HTML+CSS** (no framework, no build
   step) to a working dir, sized to the component's **point** dimensions. Use the
   AX inventory for structure/text and the reference shot for colors, spacing, and fonts.
   **🔑 Single pixel source:** sample EVERY colour and slice EVERY sprite from the **same current
   capture you will diff against** — for a selected component that is the pick-time `crop_path`
   (set the generator's source image to the crop *before* any sampling/slicing). NEVER read a
   colour from a cached constant or an older capture; that's the `#1b1b1b→#282828` stale-bg trap.
   If you reuse a prior generator (step 2c), confirm its `C_*` colour samples and sprite slices
   read from THIS run's image, not the one baked at its last run.
   **Sample colors with `sample_color` at MULTIPLE points — don't assume one flat fill.**
   Backgrounds vary: a title-bar strip, a header band, and the body can be different shades
   (e.g. Slack's lighter title bar over a darker aubergine body) — sample each region; guessing
   a single bg color is a common, visible miss. **Sample the fill that shows in the GAPS between
   icons too** (the container background) — that's the one that bit us as a too-dark ribbon.
   For **icons and images**, drop in the real assets from step 3b (inline the harvested SVGs /
   reference the saved PNGs + woff2). 🚫 **NEVER hand-draw an icon. If you find yourself about to
   write an SVG path from scratch, STOP and harvest or crop the real one** (step 3b's exact-icon
   fallback) — a hand-drawn approximation is never acceptable, even for a "simple" glyph. A tiny
   generator script that reads the manifest and substitutes assets by name keeps this repeatable.

   **Emit SEMANTIC, interactive markup — never one flat image or all `<div>`s.**
   Every control is an *individual, real, focusable element* chosen by its
   accessibility **role**, mapped through the shared, platform-agnostic
   `_shared/role-map.json` (via `_shared/markup.py`'s `role_el`/`el`; AX roles *and* ARIA/DOM roles → tag):
   `AXButton`/`AXMenuButton`/`AXCheckBox` → `<button>` (with `aria-haspopup` /
   `aria-pressed`); a radio in a tab group → `<button role="tab" aria-selected>`;
   `AXComboBox` → `role="combobox"` + `<input>`; `AXTextField`/`AXSearchField` →
   `<input type=text|search>`; `AXSlider` → `role="slider"`; `AXStaticText` →
   `<span>`. The icon artwork (sprite PNG for AX apps, inline SVG for Electron)
   goes **inside** the real control. Inline `_shared/interactive.css`
   for the button/input reset (so wrapping is a pixel no-op) + hover/focus/active.
   This is **platform-agnostic** — drive it from the role, never special-case per
   app. Static clone: set `aria-selected`/`aria-pressed` at build time; no JS.

   **Lay it out as a NESTED HIERARCHY in NORMAL FLOW — not a flat `position:absolute`
   canvas.** Emit real landmark containers (`<header>`/`<nav>`/`<aside>`/`<main>`/
   `<section>`) and place children with flexbox + normal flow (`display:flex`, `gap`,
   `margin`, `margin-left:auto`, `padding`, `flex` ratios) — *not* per-element
   `left`/`top`. Reserve `position:absolute` for **genuine overlays pinned inside a
   `position:relative` parent** (a presence dot on an avatar corner, a dropdown caret,
   a focus ring) — never as the primary layout mechanism. This holds even when the
   source gives you pixel bounds (AX tree / screenshot): translate those bounds into
   container `padding`/`gap`/`margin` **once**, computed from the measured constants,
   so the markup reads like hand-written code and stays accessible/maintainable. Two
   techniques keep it pixel-exact: **anchor each group/section by its fixed edge**
   (toolbar right-group via `margin-left:auto` + a fixed end-padding; let intra-group
   `gap`s fall inward) and **re-anchor long vertical lists per-section with an explicit
   `margin-top`** (so flex rounding resets per section instead of accumulating).
   Center text with the row container's `align-items:center` + leaf `line-height:1`
   (font-metric-robust; avoids per-element vertical offsets). These techniques are
   **app-agnostic** — a DOM-sourced app (Slack), a screenshot/AX-opaque one (Acrobat), and a
   dense AX one (Excel: nested ribbon groups, a recursive x/y→margin renderer, one A1 overlay)
   all nest + flow through the same `_shared/markup.py` + `role-map.json`. There are **no
   per-app generators to copy**: apply the method here, writing the HTML directly or via one
   throwaway generator in the output dir that imports the toolkit. See the playbook for worked
   patterns.

   **Grid labels (column letters / row numbers / a date strip baked into one image) →
   REAL TEXT, never a sprite.** Use the shared `_shared/gridtext.py` (AX →
   OCR → sequence-repair): detect the range + cell sizes from the capture's gridlines
   (never hardcode the count), get each label from AX if exposed else OCR each cell (macOS
   Vision via `_shared/vision_ocr.swift`) and repair with a `kind` (alpha/numeric) — OCR
   gives the start (scroll-aware), the kind only cleans noise + fills misses. Render real
   `<div>` cells; detect the selected row/col highlight. Reuse across apps; Excel headers
   are the reference. (Real text costs a little SSIM vs the sprite in dense bands — expected.)

7. **Render.** Via the `playwright` MCP:
   - `browser_resize { width: point_w, height: point_h }`
   - `browser_navigate { url: "file:///abs/path/to/replica.html" }`
   - `browser_take_screenshot { filename: "replica-<component>.png" }` → note the saved path.

8. **Verify.** `compare_images { a_path: <native crop path>, b_path: <replica path> }`.
   Read `score` (0..1) and look at the returned **diff heatmap** (bright red = where
   they differ).

9. **Iterate — YOURSELF, to the bar. The dev picks once and never iterates.** If `pass` is false
   (score < 0.92): inspect the heatmap, fix the HTML/CSS for the highlighted regions, re-render,
   re-diff. **Run EVERY iteration autonomously** — re-sampling colours, re-slicing sprites, and
   nudging alignment are all YOUR job, done from the crop you already have. 🚫 **NEVER stop at a low
   score to hand the loop back to the dev, and NEVER ask the dev to do something the crop already
   enables** (bring the app forward, confirm a colour, pick again). The dev performed exactly one
   action — the pick — and is owed the finished, verified component. With pixels sampled fresh from
   the crop (step 6), the first render is already close, so this is a few quiet internal rounds, not
   a dev round-trip. Stop when:
   - `score ≥ 0.92` (pass), **or**
   - **6 iterations** on this component, **or**
   - two consecutive iterations improve `score` by **< 0.005** (diminishing returns).
   Report the final score per component; never silently accept a low score — but reaching the stop
   condition is YOUR call to make and report, not a question to bounce to the dev.

10. **Compose & final-verify.** Assemble verified components into the full page at
    the window's point size, render, and `compare_images` against the
    `screenshot_window` shot. Report the final whole-window score and write the
    final `index.html`.

11. **Pre-done asset check (REQUIRED — do not skip).** Before you report the replica as done,
    audit the markup: **every icon, avatar, image, and font must be a real harvested asset** (from
    `extract_assets` / the cache) or a pixel-crop of the real element — **zero hand-drawn or
    placeholder assets** (no improvised `<svg><path>`, no initials-in-a-box avatars, no guessed
    glyphs). Re-look at the diff heatmap with the icon/background regions specifically in mind. **Also
    confirm every colour — especially the container/gap background — was sampled from THIS run's
    capture (the fresh crop), not a cached constant or older capture.** The bar is **exact assets and
    exact colours on the first attempt** — the user should never have to tell you an icon, background,
    or colour is wrong. If anything is still approximate, fix it (harvest/crop/re-sample) before
    declaring done.

## vibe-extract tools (reference)
`check_ax_permission`, `request_ax_permission`, `frontmost_app`, `list_windows`,
`ax_tree`, `ax_node_at_point`, `ax_subtree_at_point`, `screenshot_region`,
`screenshot_window`, `sample_color`, `color_palette`, `relaunch_with_debug_port`
(destructive — needs `confirm:true`), `extract_component`, `extract_assets`
(real fonts/icons/images via CDP → local files + manifest), `compare_images`.

## Notes
- Prefer `ax_tree` node `bounds` → `screenshot_region` for pixel-tight crops over
  `screenshot_window` cropping.
- Mask dynamic content (clocks, unread badges) mentally when reading the score; a
  perfect static replica may still show small red regions there.
- If `compare_images` scores low even on a good replica, re-check you rendered at
  **point** size (not device px) — that's the #1 cause.
