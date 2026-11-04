# remage

A keyboard-centric image viewer for Linux, written in Rust. It aims for nsxiv feature parity with
vim-style control and the Nothing OS design language. It is GPU-rendered so it feels smooth, and it
can be themed from the wallpaper (matugen or pywal). Scripting works through declarative commands
plus an IPC socket. The binary and crate are both `remage`, and the Wayland app_id / X11 class is
`remage`.

## Principles (in priority order)

1. **Fast.** Cold start to the first painted image takes < 200 ms. `n`/`p` to a preloaded image
   takes one frame. CPU use is 0% when idle: render on demand only, with no continuous loop.
2. **Smooth but light.** Draw with wgpu and use short ease-out animations. No GUI toolkit and no
   embedded interpreter. Animation runs only while something moves.
3. **Keyboard first.** Every feature is an *action*. Every action can be bound to a key and is
   reachable from the `:` palette and over IPC. The mouse works but is secondary.
4. **Everything configurable** in TOML, with live reload.
5. **Never block the UI thread** on I/O, decoding or scripts. A broken image or config never panics.
   The error shows in the status bar instead.

## Tech stack

Use the latest stable crate versions at implementation time (`cargo add`) and never guess version
numbers. Check APIs on docs.rs, especially winit, wgpu and glyphon, which break between releases
and must stay mutually compatible.

| Concern | Choice |
|---|---|
| Window / input | `winit` (ApplicationHandler API), Wayland + X11, no decorations by default |
| GPU | `wgpu`: Vulkan + GL backends, `PowerPreference::LowPower` (an iGPU is plenty, and waking a sleeping laptop dGPU costs ~2 s; `WGPU_POWER_PREF`/`WGPU_BACKEND` override). `PresentMode::Fifo`, `desired_maximum_frame_latency = 1`, call `pre_present_notify()` before present |
| Text | `glyphon` (cosmic-text) with embedded fonts |
| Decoding | `image` (png/apng, jpeg, gif, webp, tiff, bmp, ico, tga, pnm, qoi, dds, hdr, exr), `jxl-oxide`, `image/avif-native` (dav1d), `libheif-rs`, `resvg`, `rawler` |
| Colour | `moxcms` (ICC → sRGB, CICP); EXIF orientation from `image` / `kamadak-exif` |
| Thumbnails | `fast_image_resize` (SIMD), `png` + `md-5` for the freedesktop cache |
| Config | `serde` + `toml` (spans give `file:line:col` errors) |
| Fuzzy matching | `nucleo-matcher` |
| Watching / signals | `notify` (config, includes, directories), `signal-hook` (SIGUSR1 = reload) |
| CLI | `clap` derive + `clap_complete` + `clap_mangen` |
| Concurrency | std threads + `crossbeam-channel`; workers wake the loop via `EventLoopProxy` |
| IPC | std `UnixListener` + `serde_json` |
| Errors / logs | `anyhow`, `thiserror`, `log` + `env_logger` (`RUST_LOG=remage=debug`) |

**Locked pairings (M0, 2026-11):** glyphon 0.12 ↔ wgpu 30 (cosmic-text 0.19). winit stays on
0.30 until 0.31 leaves beta. Bump glyphon and wgpu together, never one alone. Crate still to add
when its milestone needs it: `clap_mangen` (M7).

Cargo features are `default = ["jxl", "svg", "avif", "heif", "raw"]`. The only C dependencies
(dav1d, libheif) sit behind `avif` and `heif`.

Build profiles:
- Release: `lto = "thin"`, `codegen-units = 1`.
- Dev: `[profile.dev.package."*"] opt-level = 3`. Unoptimised decoders are unusably slow.

Rust 2024 edition, stable toolchain.

## Architecture

```
src/
  main.rs          CLI → App::run;  `remage msg …` client path
  cli.rs           clap definitions
  app.rs           winit ApplicationHandler: routes events, owns State + Renderer
  action.rs        Action enum + parser/Display (one grammar for keys, palette, IPC, scripts)
  scan.rs          CLI paths → file list (siblings, recursion, hidden files, unknown extensions to sniff)
  state/           PURE logic, no winit/wgpu types → unit-testable
    mod.rs         AppState: mode, pending input, status line
    queue.rs       file list, index, marks, sort, filter, refresh
    view.rs        zoom/pan/fit/rotate/flip maths, animation targets
    grid.rs        grid layout, cursor, scroll, visual range
  input/
    keys.rs        vim key-notation parser (<C-x>, <S-Tab>, gg, <Space>w, mouse)
    keymap.rs      per-mode trie, pending sequences, counts, which-key candidates
  config/
    mod.rs         schema, compiled-in defaults, include merge, validation, live reload
    theme.rs       tokens, presets, palette derivation, pywal import
  decode/
    mod.rs         sniff (magic bytes, then extension) → backend → DecodedImage{frames, icc, orientation, meta}
    *.rs           image_rs, jxl, svg, heif, raw, external
    color.rs       ICC/CICP → sRGB, orientation, HDR tone-map
  worker.rs        priority job pool with epoch cancellation
  cache.rs         decoded-image LRU (byte budget) + preload policy
  thumbs.rs        freedesktop thumbnail cache read/write + generation
  render/
    mod.rs         device/surface setup, frame orchestration
    image.rs       textures, tiling above max_texture_dimension_2d, mipmaps, filters
    prims.rs       one instanced SDF shader: rects, rounded rects, borders, dots
    text.rs        glyphon + embedded fonts
    shaders/*.wgsl image (gamma/brightness/contrast, alpha dots), prims
  ui/              statusbar, grid view, palette, which-key, help, info panel
  anim.rs          retargetable ease-out tweens; requests redraws only while active
  exec.rs          user commands: template expansion, spawn, confirm, output handling
  ipc.rs           socket server, JSON protocol, event subscriptions
assets/fonts/      SpaceGrotesk, SpaceMono, Doto + OFL licences
contrib/           config.example.toml, matugen/, pywal/, scripts/, remage.desktop, PKGBUILD
tests/fixtures/    tiny images per format, EXIF orientations 1–8, ICC-tagged, animated
```

**Data flow.** Input goes through the keymap, which produces an `Action`. `State::apply` turns the
action into effects: jobs, spawns, redraw. Workers send `UserEvent`s back through the proxy, and
those update the state and renderer. The renderer reads state and never owns logic.

**Threads.**
- **Main thread:** event loop, state, rendering and GPU uploads. Uploads are budgeted per frame to
  about 32 MB so large textures never cause a hitch.
- **Decode pool:** `nproc − 1` workers, ordered by priority:
  - P0: current image
  - P1: neighbours (preload)
  - P2: visible thumbnails
  - P3: other thumbnails

  Every job carries an epoch, so mashing `n` never decodes images that were skipped.
- **Filesystem thread:** directory listing, sniffing and the notify watcher.
- **IPC thread:** accepts socket connections and forwards actions through the proxy.
- **Child processes:** run async. Their exit status and stdout come back through the proxy.

**Startup.**
1. Parse the CLI and config.
2. Start decoding the first image right away.
3. In parallel, create the window hidden and initialise wgpu.
4. Show the window on the first finished frame, so it never flashes empty.

The directory listing continues in the background; only the first image is awaited.

## Feature spec

### Queue
- `remage` with no arguments is the same as `remage .`. A directory argument loads its images,
  non-recursively unless `-r` is given. A single file argument loads its siblings and starts on that
  file (`general.open_siblings`, or `--no-siblings` to turn it off).
- Known extensions are accepted instantly. Unknown ones are sniffed in the background. Undecodable
  files drop out of the queue, and the status line reports `[SKIPPED n]`.
- Sorting is `natural` (the default), `name`, `mtime`, `size` or `random`, plus `reverse`. With
  `general.watch_dir`, files that appear or disappear on disk are added or removed live.
- `-i` reads paths from stdin (`-0` for NUL-separated). `-o` prints marked files on exit.

### Modes
- **Image:** shows a single image.
- **Grid:** nsxiv's thumbnail mode, the "menu mode" from doc.md. `<CR>` toggles between image and
  grid. **Visual** is a sub-mode of grid: press `v`, move to extend a range, then `m` marks it.
- **Prompts:**
  - `:` opens the command palette, which fuzzy-matches all actions and user commands and shows their
    bound keys.
  - `/` opens fuzzy filename search. It filters the grid live, and `<CR>` jumps to the selected file.
  - The confirm prompt asks inline (`DELETE 3 FILES?  Y / N`).
- **Overlays:**
  - Which-key appears after a pending prefix has waited `ui.which_key_delay_ms` (300 ms).
  - `?` shows help, generated from the live keymap.
  - `i` shows the info panel with EXIF and metadata.

### Image view
- **Fit modes:** `down` (the default, as in nsxiv), `fit`, `fill`, `width`, `height` and
  `original`.
- **Zoom:** happens around the cursor for the mouse and around the centre for keys. Zoom
  interpolates in log space with the anchor held fixed.
- **Panning:** `hjkl` pans and `HJKL` jumps to an edge. A held key pans at constant velocity from
  key-down to key-up, never in key-repeat steps.
- **Rotate and flip:** 90° rotation and h/v flips change only the view, never the file. EXIF
  orientation is applied automatically.
- **Filtering:** linear with mipmaps when shrinking. Above 300% it switches to nearest automatically
  for pixel art, and `a` toggles it by hand.
- **Adjustments:** gamma, brightness and contrast are shader uniforms.
- **Transparency background:** `dots` (the Nothing dot-matrix grid, default), `checker` or `solid`.
- **Animation and pages:** GIF, APNG, WebP and JXL animations play with their own timing.
  `<C-Space>` pauses and `<C-n>`/`<C-p>` step frames. Multi-page TIFFs step the same way.
- **Slideshow:** `s` toggles it, with a configurable delay.
- **Huge images and SVG:** images larger than `max_texture_dimension_2d` are tiled. SVGs are
  re-rasterised at the current zoom (debounced) so they stay sharp.
- **Preload and cache:** the `image.preload` images on each side are decoded *and* uploaded to the
  GPU. Decoded images live in an LRU capped by `image.cache_mb`.
- **Image switching:** a 100 ms crossfade, or none. `[LOADING]` appears only if loading takes more
  than 120 ms, so it never flickers.

### Grid
- **Thumbnail cache:** uses the freedesktop thumbnail cache in `$XDG_CACHE_HOME/thumbnails/`, with
  `normal` 128, `large` 256, `x-large` 512 and `xx-large` 1024.
  - Each entry is named by the MD5 of the file URI and validated by `Thumb::URI` + `Thumb::MTime`.
  - remage picks the size bucket ≥ thumb size × scale factor.
  - Writes are atomic (tmp + rename) with mode 0600, and failures go in `fail/remage/`.
  - `-p` disables writing. Because the cache is shared with file managers, thumbnails are often
    already there.
  - URIs are escaped exactly as GLib's `g_filename_to_uri` escapes them (`!$&'()*+,;=:@` stay
    literal, hex is uppercase), or the names won't match other tools' entries. A bare path in
    `Thumb::URI` (ffmpegthumbnailer writes those) also counts as valid.
  - A `fail/remage/` entry is trusted only if its `Software` is this remage version, so formats a
    newer version can decode aren't hidden forever.
  - Images no bigger than the bucket aren't written: re-decoding them costs about as much.
- **Fast paths:** JPEG uses a DCT-scaled decode (`jpeg-decoder` `Decoder::scale`). RAW uses the
  embedded preview. Then `fast_image_resize` finishes the job (Lanczos, premultiplied alpha).
- **Generation and storage:**
  - Visible thumbnails are P2. A page above and two pages below are P3. Requests that scroll out of
    that range are cancelled (`Pool::cancel`).
  - On the GPU, thumbnails live in 2048² atlas pages at 1:1 pixels, each with a 1 px clear border.
    The least recently drawn thumbnail outside the prefetch range is evicted first; if every one is
    in range, the atlas grows a page. A new grid size or scale factor rebuilds the atlas.
  - Thumbnails fade in over 150 ms. A thumbnail that fails drops the file from the queue (counted in
    `[SKIPPED n]`), as in image mode.
- **Cursor:** the grid cursor *is* the queue's current image, so `n`/`p`/`gg`/`G` move it too. The
  full image decodes only once the cursor has rested 250 ms (`GRID_REST`). That way `<CR>` opens it
  at once without decoding everything the cursor passes over.
- **Navigation:** `hjkl`, `gg`/`G`/`[count]G`, `<C-d>`/`<C-u>`, `<C-f>`/`<C-b>` and `0`/`$`.
  Scrolling animates smoothly, and `+`/`-` steps through `grid.sizes`. The wheel scrolls by rows,
  pixel-exact on touchpads.
- **Visual mode:** `v` anchors a range and movement extends it. `m` (`mark on`) marks it and returns
  to the grid; `mark` toggles, unmarking a range that is all marked. `v` or `<Esc>` leaves. The range
  sits on `surface_raised`, and the bar shows `[VISUAL n]`.

### Marks
- `m` toggles a mark, `M` inverts all marks and `u` unmarks everything.
- `N`/`P` jump to the next/previous marked image.
- Marks are the default target of user commands, and `-o` prints them on exit.
- Marks are kept by path, so sorting and reversing keep them.
- In the status bar, an accent dot plus the count shows the marks. The dot is filled when the
  current image is marked and a ring when it isn't.

### Input
- **Key notation** follows vim: `n`, `G`, `<C-x>`, `<S-Tab>`, `<Space>`, `<BS>`, `<Esc>`, `<CR>`,
  `<Left>`, `<F5>`. Sequences work too, such as `gg` and `<Space>w`.
- **Mouse notation:** `<ScrollUp>`, `<C-ScrollUp>`, `<LeftMouse>`, `<RightMouse>`, `<MiddleMouse>`.
  Drag-to-pan is built in.
- **Tables:** `[keys.global]`, `[keys.image]`, `[keys.grid]` and `[keys.visual]`. A mode's table
  overrides `global`.
- **Binding values:** an action string, an array of actions (run in order, with `run` awaited before
  the next one), or `"none"` to remove a default binding.
- **Counts:** a digit prefix becomes the count (`5n`, `12G`, `3+`). `0` acts as a motion when no
  count is pending. A pending prefix or count shows in the status bar.

### Actions

Actions share one grammar, `name [args]`, used by keys, the palette, IPC and command stdout. Each
action carries a one-line description, which feeds the palette, which-key and help.

| Group | Actions |
|---|---|
| Navigation | `next [n]`, `prev [n]`, `first`, `last`, `goto N`, `random`, `next-mark`, `prev-mark`, `next-frame`, `prev-frame` |
| Marks | `mark [toggle\|on\|off]`, `mark-all`, `unmark-all`, `invert-marks` |
| Grid | `move <dir> [n]`, `page down\|up\|half-down\|half-up`, `row-start`, `row-end`, `scroll <dir> [n]` (pans a step in image mode) |
| View | `zoom in\|out\|N%`, `fit <mode>`, `pan <dir>`, `pan-edge <dir>`, `center`, `rotate cw\|ccw\|180`, `flip h\|v`, `filter toggle`, `alpha-bg cycle`, `gamma\|brightness\|contrast +\|-\|reset` |
| Playback | `play toggle`, `slideshow toggle`, `slideshow-delay ±N` |
| UI | `mode image\|grid\|toggle`, `visual`, `fullscreen`, `bar toggle`, `info toggle`, `help`, `palette`, `search`, `grid-size +\|-` |
| Queue | `sort <key>`, `reverse`, `filter <pattern>`, `filter-clear`, `remove`, `reload`, `refresh`, `open <path>` |
| App | `run <command>`, `reload-config`, `echo <msg>`, `quit`, `quit-no-output` |

### Commands and scripting (replaces nsxiv's key-handler)

```toml
[commands.wallpaper]
desc = "Set as wallpaper"
run  = ["setwall", "-p", "{file}"]

[commands.copy]
desc = "Copy image"
run  = "wl-copy --type image/png < {file}"   # string = sh -c, placeholders shell-quoted

[commands.delete]
desc    = "Delete files"
run     = ["rm", "--", "{files}"]             # array = exec directly, {files} → N args
confirm = "Delete {count} file(s)?"

[commands.legacy]                             # an old nsxiv key-handler works unchanged
run   = ["~/.config/nsxiv/exec/key-handler", "w"]
stdin = "files"

[keys.global]
"<Space>w" = "run wallpaper"
"<Space>c" = "run copy"
"<Space>d" = "run delete"
```

**Command fields:**
- **Placeholders:** `{file} {files} {dir} {name} {stem} {ext} {index} {count} {width} {height}`.
  `{file}`, `{dir}`, `{name}`, `{stem}`, `{ext}`, `{index}`, `{width}` and `{height}` describe the
  current image; `{files}` and `{count}` (and `REMAGE_COUNT`) describe the targets. Only these
  names are substituted, so `awk '{print $1}'` survives. In an exec array, an element containing
  `{files}` repeats once per target (`--in={files}` → `--in=a --in=b`), and a leading `~/` in the
  template expands. In a shell string every value is shell-quoted, so don't quote placeholders.
  A command that mentions file placeholders (or reads stdin) refuses to run with no targets.
- **`targets`:** `auto` (marked files if any, otherwise the current one; the default), `current`,
  `marked` or `all`.
- **`stdin`:** `none`, `files` (newline-separated) or `files0` (NUL-separated).
- **Environment:** `REMAGE_FILE`, `REMAGE_INDEX`, `REMAGE_COUNT`, `REMAGE_MODE`, `REMAGE_SOCKET`,
  `REMAGE_PID`.
- **`confirm`:** turns the command into an inline y/n prompt. remage has no modal dialogs. `y`
  runs it and any other key (or `cancel`) drops it. The prompt shows even with the bar hidden.
- **`output`:**
  - `status` (default): the last stdout line goes to the status line.
  - `actions`: every stdout line is run as an action.
  - `ignore`.
- **Running:** commands always run async. The status bar shows `[RUNNING WALLPAPER]`, then `[DONE]`
  or `[ERROR: <code> <first stderr line>]`.
- **`after`:** actions to run on success. The default is `refresh`, which re-stats the targets,
  drops missing files and re-decodes changed ones. That way delete and rotate scripts work with no
  extra setup. "Changed" means the mtime differs from the one recorded when the file was decoded
  or thumbnailed. A bare `refresh` (key, IPC) re-checks the whole queue.
- **Chains:** in a binding array, the actions after a `run` (and the command's `after`) wait for
  it to exit successfully; a failure or a declined prompt drops the rest.
- **Unbound commands** still appear in the `:` palette.

**IPC socket.** It lives at `$XDG_RUNTIME_DIR/remage/<pid>.sock` and speaks newline-delimited JSON.
- **Requests:**
  - `{"action":"goto 5"}`
  - `{"query":"current"|"marked"|"queue"|"state"}`
  - `{"subscribe":["image","marks","mode"]}`, which turns the connection into an event stream.
- **Replies:** `{"ok":true,"data":…}` or `{"ok":false,"error":"…"}`.
- **Client:** `remage msg <action>`, `remage msg --query marked` and `remage msg --subscribe image`.
  It targets `$REMAGE_SOCKET`, otherwise the newest instance; `--pid` picks one explicitly.
  Strings and string lists print one per line, anything else as JSON; errors go to stderr with
  exit 1. Subscriptions print one JSON event per line.
- **Details:** `"action"` also takes an array. Events are `image` (`path`, 1-based `index`,
  `count`), `marks` (`count`, `files`) and `mode`. The socket is 0600 in a 0700 directory,
  sockets of dead pids are cleaned up at startup, and the socket is removed on exit once any
  in-flight reply (to a `quit`) has gone out.
- **Uses:** Hyprland binds, long-running watchers and other tools driving the viewer.

### Config
- **Location:** `$XDG_CONFIG_HOME/remage/config.toml`, falling back to `~/.config/remage/`. `-c`
  overrides it.
- **Defaults:** every option has a compiled-in default, and the user file deep-merges over them, so a
  partial file is fine. `remage --dump-config` prints the full commented default.
  `remage --check-config` validates the file and exits non-zero with `file:line:col` errors.
  Unknown keys are errors and come with a "did you mean" suggestion.
- **`include`:**
  - Format: `include = ["~/.cache/matugen/remage.toml"]`. Paths expand `~` and `$VAR`, and relative
    paths resolve against the config dir.
  - Semantics follow alacritty's `import`. Includes load in order, and a missing include is a
    warning, not an error. The including file loads last and wins.
- **Live reload:** a notify watch on the config and every include, debounced to 100 ms. SIGUSR1 and
  the `reload-config` action also reload. An invalid config keeps the last good one and shows
  `[CONFIG ERROR: …]`. Theme changes crossfade over about 250 ms, so a wallpaper change recolours
  smoothly.
- **Sections:** `general`, `window`, `image`, `grid`, `ui`, `animation`, `theme`, `keys.<mode>`,
  `commands.<name>` and `[[decoders]]`. Representative keys:

```toml
[general]   open_siblings = true; recursive = false; sort = "natural"; start_mode = "image"; wrap = true; watch_dir = true
[window]    decorations = false; title = "{name} — remage"; class = "remage"; fullscreen = false
[image]     fit = "down"; zoom_step = 1.25; filter = "auto"; alpha_background = "dots"; auto_orient = true
            color_management = true; preload = 2; cache_mb = 512; slideshow_delay = 5.0
[grid]      sizes = [96, 128, 160, 224, 320]; size = 160; gap = 16; thumbnail_cache = true; names = false
[ui]        statusbar = true; which_key_delay_ms = 300; scale = 1.0
            statusbar_left = "{name}"; statusbar_right = "{format}  {width}×{height}  {zoom}%  {index}/{count}"
            info_command = ""   # optional script whose output fills a status segment (nsxiv image-info)
            font_ui = "Space Grotesk"; font_mono = "Space Mono"; font_display = "Doto"
[animation] enabled = true; duration_ms = 180; transition = "fade"
[theme]     preset = "nothing-dark"
```

### Theming
- **Tokens:** `background`, `foreground`, `surface`, `surface_raised`, `border`, `border_visible`,
  `text_disabled`, `text_secondary`, `text_primary`, `text_display`, `accent`, `success`,
  `warning` and `image_background` (defaults to `background`).
- **Presets:** `nothing-dark` is the default (`#000000` / `#FFFFFF` / accent `#D71921`).
  `nothing-light` is `#F5F5F5` / `#000000` / `#D71921`.
- **Derivation:** only `background`, `foreground` and `accent` are needed. remage derives any token
  that is missing by mixing fg over bg in sRGB:

  | Token | fg share |
  |---|---|
  | `text_display` | 1.0 |
  | `text_primary` | 0.91 |
  | `text_secondary` | 0.60 |
  | `text_disabled` | 0.40 |
  | `border_visible` | 0.20 |
  | `border` | 0.133 |
  | `surface_raised` | 0.10 |
  | `surface` | 0.067 |

  These ratios reproduce the Nothing hex tokens exactly, and they give any wallpaper palette a
  coherent four-level hierarchy.
- **matugen:** ship `contrib/matugen/remage.toml`:
  ```toml
  [theme]
  background = "{{colors.surface_container_lowest.default.hex}}"
  foreground = "{{colors.on_surface.default.hex}}"
  accent     = "{{colors.primary.default.hex}}"
  ```
  Register it in `~/.config/matugen/config.toml` with
  `[templates.remage] input_path = '~/.config/matugen/templates/remage.toml'` and
  `output_path = '~/.cache/matugen/remage.toml'`. Then set
  `include = ["~/.cache/matugen/remage.toml"]` in the remage config. The live reload picks up every
  wallpaper change, so no `post_hook` is needed.
- **pywal:** read it natively with `[theme] pywal = "~/.cache/wal/colors.json"`. That maps
  `special.background` / `special.foreground` / `colors.color1` to background / foreground / accent.
  A template is also provided in `contrib/pywal/`.

### UI design language (Nothing)

**Load the `nothing-design` skill before any UI work.** These are the remage-specific rules:
- **Fonts:** embedded, all under the OFL; copy them from `~/.local/share/fonts/glyph/` along with
  the licence.
  - Space Grotesk for UI text.
  - Space Mono for labels and data, in ALL CAPS with 0.08em tracking.
  - Doto (dot-matrix) only at 36 px and above, for hero moments.

  A screen uses at most 2 families, 3 sizes and 2 weights.
- **The image is the hero.** There is no chrome except the optional status bar (`b`), which sits at
  the bottom on a solid background with no border.
  - **Left side:** the filename in Space Grotesk 14, `text_primary`.
  - **Right side:** Space Mono caps 11, `text_secondary`, for example
    `JPEG  4032×3024  72%  012/240`.
  - **Marks:** an accent dot plus the count, shown only when the count is > 0.
  - **Pending keys:** shown in `text_disabled`.
  - **Messages:** inline (`[COPIED]`, `[ERROR: …]`). Never toasts.
- **Accent red is a signal only:** marks, errors and the palette's selection bar. Nothing else on
  screen is coloured.
- **One surprise per screen.** A count prefix or `goto` shows as large Doto numerals in a corner.
  The empty state is Doto `NO IMAGES` plus a single hint line.
- **Overlays** (palette, which-key, info, help) use a `surface` background, a 1 px `border_visible`
  border and a radius of 8–16, with no shadow or blur. The selected row is `surface_raised` with a
  2 px accent bar on the left. Overlays anchor to an edge (bottom or right), never a centred modal.
- **Grid:**
  - Gaps are 16 px.
  - The cursor is a 1 px `text_display` outline with a 4 px radius.
  - A marked thumbnail gets an accent dot.
  - A pending thumbnail is a single dim dot. No skeletons.
- **Motion:** ease-out `cubic-bezier(0.25, 0.1, 0.25, 1)` over 150–250 ms. Fade rather than slide,
  with no spring or bounce. Every animation can be retargeted from its current value, and
  `animation.enabled = false` turns them all off.

### Formats

| Formats | Backend |
|---|---|
| JPEG, PNG/APNG, GIF, WebP (animated), BMP, ICO, TIFF (multi-page), TGA, PNM, QOI, DDS, HDR, OpenEXR | `image` (pure Rust) |
| JPEG XL (incl. animation) | `jxl-oxide` (pure Rust) |
| AVIF | `image` `avif-native` (system dav1d) |
| HEIC / HEIF | `libheif-rs` (system libheif) |
| SVG / SVGZ | `resvg`, re-rasterised on zoom |
| Camera RAW (CR2/CR3/NEF/ARW/DNG/RAF/ORF/RW2…) | `rawler`: the embedded preview first (fast), with a full develop on demand |
| Anything else (PSD, XCF, JP2, …) | `[[decoders]]` entries `{ match = ["*.psd"], run = ["magick", "{file}[0]", "png:-"] }`, which emit PNG/PPM/TIFF on stdout |

Pipeline rules:
- Detect the format by magic bytes first and the extension second.
- Convert ICC and CICP colour to sRGB.
- Tone-map 16-bit and float images (EXR/HDR) to 8-bit sRGB for display.
- Apply EXIF orientation.

### CLI

```
remage [OPTIONS] [PATHS]...
  -r --recursive   -i --stdin   -0 --null   -o --print-marked   -t --grid
  -n --start N     -f --fullscreen   -b --no-bar   -S --slideshow [SECS]
  -s --fit MODE    -z --zoom PCT     -g --geometry WxH   -c --config PATH
  -p --private     --class NAME      --sort KEY  --reverse   --no-siblings
  --dump-config    --check-config
remage msg [--pid PID] [--query Q | --subscribe EV] [ACTION...]
remage completions <SHELL>
```

### Default keys

| Mode | Keys |
|---|---|
| global | `q` quit · `<CR>` image⇄grid · `n`/`p`/`<BS>` next/prev · `[`/`]` ±10 · `gg`/`G`/`[N]G` · `m`/`M`/`u` mark/invert/unmark-all · `N`/`P` next/prev mark · `f` fullscreen · `b` bar · `i` info · `r` reload · `D` remove from queue · `s` slideshow · `:` palette · `/` search · `?` help · `<Space>` leader (user commands, which-key) · `<Esc>` cancel |
| image | `hjkl`/arrows pan · `HJKL` edges · `z` centre · `+`/`-`/`=` zoom in/out/100% · `w`/`W`/`e`/`E` fit/fill/width/height · `<`/`>` rotate · `\|`/`_` flip · `a` filter · `A` alpha bg · `{`/`}` gamma · `<C-Space>` play/pause · `<C-n>`/`<C-p>` frame · wheel = zoom at cursor, drag = pan |
| grid | `hjkl` move · `<C-d>`/`<C-u>` half page · `<C-f>`/`<C-b>` page · `0`/`$` row ends · `+`/`-` thumb size · `v` visual · click = select, double-click = open |

## Milestones

Each milestone should end with a usable binary.

- **M0 Scaffold**
  - `git init` and `cargo init`.
  - `Cargo.toml` with the features and profiles above.
  - Fonts + OFL in `assets/fonts`, and the module skeleton.
  - fmt, clippy and test all clean.
- **M1 Viewer core** ✓ (2026-11-04). First frame in ~90 ms on the iGPU, 0% CPU when idle.
  - CLI, and a queue with siblings, natural sort and sniffing.
  - winit + wgpu window, decoding through the `image` crate.
  - Fit/zoom/pan/rotate/flip with animation, and `n`/`p`.
  - Preload + cache, a status bar with text, and the `nothing-dark` theme with hard-coded keys.
- **M2 Input + config** ✓ (2026-11-04). Unbuilt actions parse and bind, then say "not available yet".
  - Action grammar, and a keymap trie with sequences, counts and mouse bindings.
  - TOML defaults, include and live reload, plus `--dump-config` and `--check-config`.
  - Theme tokens, presets and derivation; matugen + pywal support.
- **M3 Grid + marks** ✓ (2026-11-04). Cached grid opens in ~85 ms, 0% CPU when idle.
  - Freedesktop thumbnails, worker priorities and the GPU atlas.
  - Grid navigation, visual mode, marks and smooth scrolling.
- **M4 Commands + IPC** ✓ (2026-11-04). Tested end to end over the socket and with `wtype`.
  - `[commands]` with async exec, confirm, output modes and `after`/`refresh`.
  - The socket, `remage msg` and subscriptions; `-i`/`-o`/`-0`.
- **M5 Formats + fidelity**
  - JXL, AVIF, HEIF, SVG, RAW and external decoders.
  - ICC colour, animation, multi-page, tiling + mipmaps, and HDR tone-mapping.
- **M6 Polish**
  - Palette, search, which-key, help and the info panel.
  - Slideshow, gamma/brightness/contrast and the `nothing-light` preset.
  - `watch_dir`, title and status-bar templates.
  - A performance pass against the targets.
- **M7 Packaging**
  - Man page, zsh completion and `remage.desktop` with the MimeType list.
  - README, `contrib/config.example.toml` (with the old key-handler's delete/wallpaper/copy
    migrated) and a PKGBUILD.

## Testing and verification
- **Unit tests** cover the pure modules: key-notation parsing, the keymap trie and counts, Action
  parse ↔ Display round-trips, queue operations (natural sort, marks, filter, refresh), view maths
  (fit modes, zoom anchor, rotation), config merge, include precedence and theme derivation,
  placeholder expansion and shell quoting, and thumbnail paths (URI → MD5) per the spec.
- **Decoder tests** run against `tests/fixtures/`: dimensions and pixels for each format, EXIF
  orientations 1–8, ICC-tagged images and animated frame counts.
- **Visual checks without a display:** `REMAGE_SNAPSHOT=/tmp/x.png remage …` renders each settled
  frame offscreen to a PNG as well. Use it to check the UI when the screen is locked or remote. In
  the grid, a frame counts as settled once every visible thumbnail is in the atlas. Point
  `XDG_CACHE_HOME` at a scratch directory to test cold thumbnail generation without touching the
  real cache.
- **Before every commit:** `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and
  `cargo test`.
- **Manual smoke test:** run `cargo run --release -- /mnt/media/pix` and check that:
  - `n`/`p` are instant.
  - `<CR>` opens the grid.
  - `m` marks and `<Space>w` runs a command.
  - `remage msg next` works from another terminal.
  - Changing the wallpaper recolours the window live.
  - `RUST_LOG=remage=debug` logs startup and first-frame timings.
  - CPU is idle while the view is static.

## Conventions
- Never put Claude session links or `Claude-Session:` trailers in commits or PRs. Commit only when
  the user asks, with imperative, concise messages.
- No `unwrap()`/`expect()` on paths that touch user input, files or the GPU surface. Errors go to
  the status line.
- `state/` must stay free of winit and wgpu types.
- Adding an action takes all of these: an enum variant, a parser case, a description, an optional
  default binding, an entry in `--dump-config` and a test.
