# VibeExtract — setup for devs

VibeExtract turns a **running macOS app's UI into clean, verified HTML/CSS**. You point it at
any window (or pick one component), and the `/replicate-ui` skill in Claude Code inspects it via
Accessibility + screenshots, harvests the real fonts/icons/images, generates semantic HTML+CSS,
renders it, and diffs the render against the original until they match.

Two pieces work together:

1. **The VibeExtract app** (`VibeExtract Desktop.app`) — a tray/desktop app that reads the native
   UI and runs a small local **MCP server** on `127.0.0.1:8765`. It also owns the global hotkeys
   for picking a component.
2. **The `/replicate-ui` skill** — a **self-contained** Claude Code skill that drives the
   perceive → generate → verify loop. It **bundles its own toolkit** in `_shared/` (role map +
   role→HTML helpers, CSS reset, sprite slicer, grid-text OCR, the reuse cache), so it works on
   any machine **without this repo**.

---

## 1. Install the app

1. **Airdrop** (or copy) `VibeExtract Desktop_0.2.0_aarch64.dmg`, then open it.
2. Drag **VibeExtract Desktop** into `/Applications`.
3. **First launch:** the app is internally **self-signed** (not from the App Store / not
   notarized), so macOS will block a normal double-click. **Right-click the app → Open → Open**
   in the dialog. You only do this once; afterwards it opens normally.

## 2. Grant the two permissions (once)

VibeExtract reads other apps' UI and captures their windows, so macOS gates it behind two
privacy permissions. Open **System Settings → Privacy & Security** and add **VibeExtract Desktop**
(toggle it **on**) under **both**:

- **Accessibility** — lets it read the Accessibility (AX) tree: roles, labels, bounds.
- **Screen Recording** — lets it capture window/region screenshots (without this, shots are black).

If a permission list already has an old VibeExtract row, remove it with **–**, then re-add the
current app with **+**. Then **Quit & Reopen** the app.

> The app is signed with a stable identity ("VibeExtract Dev"), so these grants **survive app
> updates** — you won't have to re-grant on every new build.

## 3. MCP server — it's automatic

The MCP server **auto-starts when the app launches** (no toggle, no env var). Confirm it's up:

```bash
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8765/mcp   # 406 = up (rejects plain GET)
```

`406` means the server is running (it only speaks the MCP protocol). To disable autostart for
local debugging, launch with `VIBE_MCP_NO_AUTOSTART=1`.

## 4. `/replicate-ui` — the app installs it for you

**You no longer copy any folders or run `claude mcp add`.** The VibeExtract app **bundles the
`/replicate-ui` skill inside itself** and, on launch, automatically:

- copies the skill into `~/.claude/skills/replicate-ui` (so it works in every project), and
- registers the two MCP servers in your user config (`~/.claude.json`): `vibe-extract` (the live
  app URL) + `playwright` (the renderer). It **merges** — your existing MCP servers are untouched,
  and it makes a one-time backup at `~/.claude.json.vibe-backup`.

So the whole setup is just: **open the app once → restart Claude Code → run `/replicate-ui`.**

There's a **"Set up /replicate-ui for Claude Code"** button in the app (under the MCP panel) to
re-run/repair this anytime; it also tells you if a prerequisite is missing.

**Two prerequisites it can't install for you** (it detects + points you to them):
- **Claude Code** itself, signed in.
- **Node.js** (https://nodejs.org) — used by the `playwright` renderer (`npx`). Python 3 ships with
  macOS; the app best-effort-installs the one Python lib it needs (Pillow).

> **Outputs are scratch.** Each replica (its `index.html`, harvested `assets/`, screenshots, sprite
> cache) is written to `.replicate-ui/<app>/` in your current working directory — gitignore it and
> delete it anytime; it's fully regenerable.

> **After you restart the VibeExtract app, reconnect the MCP in Claude:** run `/mcp` → `vibe-extract`
> → reconnect (the server restarts with the app, so the old connection goes stale). The app re-points
> the URL to the live port on each launch, so a simple `/mcp` reconnect is all that's needed.

---

## 5. Use it

### Pick a component with hotkeys (do this on the target app, not on VibeExtract)

Selection is **hotkey-driven** — there are no buttons to toggle. Go to the app you want to capture
and:

| Hotkey | Action |
|---|---|
| **⌘⇧S** | Start picking — hover highlights elements |
| **Click** | Select the hovered element (⇧-click to add more) |
| **⌘⇧E** | Extract the selection (quick in-app preview) |
| **⌘⇧X** | Capture the whole frontmost window |
| **↑ / ↓** | Widen / narrow the hovered element |
| **Esc** | Cancel picking |

(If ⌘⇧S is swallowed by a hotkey manager like Bartender/Magnet, pressing **⌘⇧E** with nothing
picked also arms pick mode.)

### Get a high-fidelity replica with Claude (the real deliverable)

1. On the target app: **⌘⇧S**, then **click** the component you want (or skip selecting to do the
   whole window).
2. Switch to **Claude Code** (in this repo) and run:

   ```
   /replicate-ui
   ```

   - If you **selected** a component, it replicates **only that component**.
   - If you **didn't**, it replicates the **whole window**.

   It produces verified `index.html` + `assets/` under `.replicate-ui/<app>/`, iterating until the
   render matches the original (≥ 0.92 similarity).

### Incremental reuse (similar pages get fast)

The **first** page of an app pays full cost (harvest assets, OCR any grid labels, verify every
component). **Pages 2..N of the same app reuse** the cached fonts/icons/images, grid labels, and
toolbar sprites, and **start from the previous verified page** — so they're much quicker. You
don't do anything special; the skill checks the cache (its bundled `_shared/cache.py`)
automatically. To see what's cached for an app:

```bash
python3 ~/.claude/skills/replicate-ui/_shared/cache.py <app>     # e.g. slack
```

---

## Troubleshooting

- **"App is damaged / can't be opened"** → it's self-signed; right-click → **Open** the first
  time (step 1).
- **Screenshots are black / AX tree empty** → the matching permission isn't granted; re-check
  step 2, then Quit & Reopen.
- **`/replicate-ui` can't reach `vibe-extract`** → the app isn't running, or you restarted it and
  Claude's connection is stale. Make sure the app is open, `curl` returns `406`, then `/mcp` →
  reconnect.
- **Hotkeys do nothing** → another app may own ⌘⇧S/⌘⇧E. Free them in that app's settings, or use
  the ⌘⇧E auto-arm fallback. Make sure VibeExtract has **Accessibility** (hotkeys + AX both need
  it).
- **A grid of labels (e.g. spreadsheet headers) renders as a blurry image** → that's the OCR
  fallback for baked-in label grids; see `docs/REPLICATE_UI_PLAYBOOK.md` → "Grid labels → real
  text".
