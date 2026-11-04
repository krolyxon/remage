# Changelog

## [0.1.0] - 2026-11-05

The first release: a keyboard-centric image viewer for Linux, at nsxiv parity with vim keys,
GPU rendering and wallpaper theming.

**Viewing**
- Opens files, directories (`-r` for trees) or a list on stdin (`-i`, `-0`); a single file opens
  its whole directory. Natural, name, mtime, size and random sort, and the queue follows files
  appearing and vanishing on disk.
- GPU-rendered with wgpu: the first image in about 90 ms, neighbours preloaded so `n`/`p` take one
  frame, and no CPU use while the view is still.
- Fit modes, zoom at the cursor, smooth panning, rotation and flips, automatic nearest filtering for
  pixel art, gamma/brightness/contrast, and a dot-matrix transparency background.
- Thumbnail grid on the shared freedesktop cache, with marks, visual-mode ranges and smooth
  scrolling.
- Slideshow, and animation playback with frame stepping.

**Formats**
- JPEG, PNG/APNG, GIF, WebP, BMP, ICO, TIFF (multi-page), TGA, PNM, QOI, DDS, HDR and OpenEXR,
  plus JPEG XL, AVIF, HEIC/HEIF, SVG/SVGZ (re-rendered sharp at any zoom) and camera RAW.
- ICC and CICP colour management, EXIF orientation, HDR tone-mapping, and tiling for images larger
  than the GPU's texture limit.
- External decoders through `[[decoders]]`, such as ImageMagick for PSD and XCF.

**Control**
- Vim key notation with sequences and counts. Every feature is an action you can bind, run from the
  `:` palette or send over IPC.
- `/` fuzzy filename search, which-key hints, `?` help built from the live keymap, and an info panel
  with EXIF data.
- User commands that replace nsxiv's key-handler: placeholders, marks as targets, inline confirm,
  output fed back as actions, and an automatic refresh afterwards.
- An IPC socket and the `remage msg` client, with queries and event subscriptions.

**Configuration**
- One TOML file with live reload, includes, `file:line:col` errors and "did you mean" hints.
- `nothing-dark` and `nothing-light` themes; matugen and pywal recolour open windows live.
- A man page (`remage man`) and shell completions (`remage completions <shell>`).

**Known gaps**
- RAW support is built but hasn't been tested on real camera files yet.
- The prebuilt binary is built on Arch Linux and needs `dav1d` and `libheif` installed. On other
  distributions, build from source.
