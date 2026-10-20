# Painter

A GPU-accelerated desktop painting app written in Rust (wgpu + egui), inspired
by the workflow of Clip Studio Paint. It is an original implementation; no
CELSYS code or assets are used.

- [docs/FEATURE_MAP.md](docs/FEATURE_MAP.md): the behaviour we target and what is done
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, GPU tile atlas, data flow, file formats
- [docs/BRUSH_IO.md](docs/BRUSH_IO.md): importing Clip Studio `.sut` and Photoshop `.abr` brushes, `.rustbrush`

## Build

Requires a stable Rust toolchain (1.88+). On Windows it also needs the MSVC
build tools ("Desktop development with C++").

```bash
cargo run --release -p painter-app
```

```bash
cargo test --workspace
```

You can pass a file to open it at startup:

```bash
cargo run --release -p painter-app -- drawing.painter
```

## Features

- **Layers:** raster, vector, folders (pass-through or isolated), paper,
  gradient and screentone layers. Each has 13 blend modes, opacity, locks,
  clipping to the layer below, and layer masks.
- **Brushes:** pressure curves for size and density (with an editor),
  per-stroke opacity, per-dab density, spacing, hardness, anti-aliasing,
  stabilisation (weighted average or pulled string), erasers. On vector layers
  the eraser removes whole strokes.
- **Tools:** rectangle, ellipse and lasso selections (new, add, subtract,
  intersect); fill bucket (tolerance, contiguous, gap closing up to 40 px, reads the edited layer, all layers or
  layers marked as reference, anti-halo expansion under the lines or inset; ~0.2 s on a 600 dpi A4 page);
  gradient tool; clear selected area.
- **Correction layers** (Layer → New Correction Layer; `painter_core::adjust`):
  brightness / contrast, levels, tone curve, hue / saturation / lightness,
  colour balance, posterize, threshold, invert and gradient map. They recolour
  everything below them, on the GPU and in exports alike, and take masks,
  opacity and clipping (clip one to a layer to correct only that layer).
- **Filters** (Filter menu; `painter_core::filters`): Gaussian, motion and
  radial blur, unsharp mask and noise on the edited raster layer, inside the
  selection, with a live preview computed off the UI thread and one undo step.
- **Rulers and guides** (Ruler tool; geometry in `painter_core::rulers`):
  1-, 2- and 3-point perspective (strokes follow the line towards the
  vanishing point they head for, plus verticals / horizontals), mirror,
  rotational and kaleidoscope symmetry (every dab repeated, vector strokes
  copied), parallel lines, concentric circles and ellipses, radial lines,
  straight rulers, horizontal and vertical guides, and a grid. Ctrl+1
  toggles snapping, Ctrl+' the grid; View → Flip Horizontally mirrors the
  view without touching the pixels. Saved with the project. Each ruler
  belongs to the layer it was made on and acts only while that layer is
  edited (or, if set, any layer in the same folder, or all layers); a ruler
  icon on the layer row shows and hides them, and rulers travel with layers
  copied to other documents.
- **GPU:** sparse tile atlas, a live stroke preview drawn with instanced dabs,
  CPU commit on a worker thread, and stroke-time composite caching. A
  headless test suite checks the GPU output against the CPU path.
- **Workspace:** a dark layout modelled on Clip Studio Paint: Brush Size
  palette, Tool Group (the toolbar group's brushes as stroke previews), Tool
  Settings, a colour wheel, layers and navigator. The toolbar on the left
  groups brushes into pen, pencil, brush, airbrush and eraser.
- **Workspaces** (crate `ui-workspace`, after Blender): tabs for Painting,
  Inking, 3D Layout, Comic and Compositing, each with its own areas and
  toolbar, plus custom copies (right-click a tab to rename, reset or delete).
  Every area has an editor-type menu, stacked tabs, Split Right / Split Down
  / Close, and borders that drag. F3 or Ctrl+Space opens a fuzzy command
  palette (menus, tools, brushes, workspaces, themes); Tab or F11 toggles zen
  mode (canvas only). Window → Theme Editor edits colours, text size and
  rounding with contrast warnings; dark, light and high-contrast presets,
  themes import and export as JSON. All of it is saved between sessions.
- **Several documents:** a tab per open document (`*` marks unsaved changes;
  Ctrl+W closes, Ctrl+Tab / Ctrl+Shift+Tab cycle), each with its own zoom,
  rotation, pan and undo history. Files open and save on worker threads.
  A Reference View area shows another open document beside the canvas (own
  zoom, pan, mirror). Layers, folders, 3D layers and selections copy into
  another document by dragging the layer onto its tab, from the tab menu or
  Layer → Copy Layer to Document, resampled to keep their print size when the
  resolutions differ (Ctrl+C / Ctrl+V across tabs does the same). File → Save
  All / Close All; quitting lists every unsaved document. Only the active
  document is on the GPU.
- **Tablet:** WinTab (pressure, tilt, automatic switch on the eraser end) with
  a fallback to Windows Ink.
- **Files:** `.painter` projects, PSD import/export, PNG open/import/export.
  The app asks before closing with unsaved changes.
- **Transform and crop** (Transform tool / Ctrl+T, Crop tool / C; crate
  `transform-tools`): the layer or selection floats under a gizmo while the
  document stays untouched, with a live GPU preview and a HUD (mode, X/Y,
  W/H %, rotation, interpolation, Apply / Cancel / Reset). The engine:
  homographies (scale / rotate / skew about a pivot, 4-corner perspective and
  free distort), a Catmull-Rom mesh warp, a toolkit-independent gizmo (8 scale
  handles, rotation handle with 15° snapping, pivot, corner nodes, mesh points,
  HUD values, Apply / Cancel / Reset), crop box (16:9, 1:1, A4, free; thirds;
  non-destructive canvas resize) and copy / cut / paste of floating
  selections. Resampling with nearest, bilinear, bicubic and Lanczos3 on the
  CPU, and the same filters on the GPU in `transform.wgsl`
  (`painter_render::TransformRenderer`) for live perspective previews; the
  `transform_parity` tests keep the two within 2/255.
- **Comic pages** (File → New Comic Page / Storyboard, Frame tool / U, the
  Panels section of the Layers panel; crate `comic-layout`): a wizard with
  format and layout previews, panel frames drawn as rectangles or polygons,
  divided with gutters, reshaped with snapping, and managed per panel (clip,
  border, order, visibility). Saved in `.painter` files (`extras/comic.json`). Print
  presets (manga B5, A4 / B5 doujinshi, webtoon strip with slice markers,
  Franco-Belgian A4 and 24 × 32 album, US comic book, Bonelli 16 × 21, custom
  with DPI, bleed, safe margin, gutters and binding side), page geometry
  (trim, bleed, inner frame, crop marks), vector panel frames with straight or
  diagonal splits and gutters, reading order for left-to-right and
  right-to-left books, snapping, frame handles, and panels as layers: a
  masked folder per panel so art stays inside (through the existing GPU mask
  compositing) plus a border layer (width, colour, anti-aliasing, dashes,
  borderless). Templates: splash, Franco-Belgian 4 tiers, US 6-panel grid,
  waffle 9, Bonelli 3 tiers, manga dynamic 6, 4-koma, webtoon stack.
  `cargo run -p painter-comic-layout --example layout_preview -- "Manga B5" "Manga dynamic 6" 150 out.png`
  renders one.
- **3D layers** (Layer → New 3D Layer from Model, 3D tool; crates `scene-3d`
  and `painter-render`'s depth-buffered scene pass): import `.blend` / `.obj`,
  Blender-style navigation (middle drag orbit, Shift pan, wheel / Ctrl zoom,
  1 / 3 / 7 views, 5 perspective, Home frame), click to pick, G / R / S gizmo,
  and a panel for transforms, colour, FOV, views and up to four lights. The
  render is copied into the layer's pixels, so it saves and exports; the scene
  itself isn't saved in `.painter` files yet. The engine: `.blend`
  reading (Blender 3.4 – 5.x, plain or zstd-compressed: blocks, the SDNA
  struct catalogue, mesh objects with UVs and transforms), an OBJ reader,
  GPU-ready meshes, a viewport camera with Blender-style navigation (orbit
  turntable / trackball, pan, zoom, dolly, roll, view presets, perspective /
  orthographic, framing), picking and a translate / rotate / scale gizmo with
  axis and plane constraints, trackball and snapping. Written from the file
  format's description and Blender's behaviour, not its GPL source.
  `cargo run -p painter-scene-3d --example preview -- model.blend out.png`
  renders a file on the CPU; `--example probe` lists its objects and structs.
- **Scanner import** (File → Import → From Scanner, crate `scanner-io`): device
  list, resolution, colour mode, preview with crop, auto-contrast and line-art
  extraction (paper → transparency) into a new layer. Works through WIA on
  Windows (enumeration, capabilities and transfer; scans run on a background
  thread); TWAIN and SANE are still stubs. `cargo run -p painter-scanner-io
  --example list_scanners` lists what WIA sees without scanning.
- **SVG export** (File → Export → Layer / Canvas as SVG, crate `vector-export`):
  layers become nested Inkscape layer groups (name, visibility, opacity, blend
  mode); vector strokes become filled outlines that keep the pressure profile,
  or centre lines; raster line art is traced into Bezier paths (threshold,
  despeckle, simplification, corner detection). Gradient layers become SVG
  gradients, tone layers dot patterns, layer masks luminance masks (traced,
  hard-edged) and clipping alpha masks reusing the base layer.
  `cargo run -p painter-vector-export --example svg_preview -- out.svg out.png`
  exports a demo document and renders it back with resvg.

## Shortcuts

| Key | Action |
|---|---|
| B / E | Brush / eraser |
| G / Shift+G | Fill / gradient |
| M / Shift+M / L | Rectangle / ellipse / lasso selection |
| H, Space + drag, middle drag | Pan |
| Wheel, Ctrl + wheel | Zoom at cursor |
| `,` / `.` | Rotate −15° / +15° |
| `0` | Fit to window |
| `[` / `]` | Brush size |
| X | Swap main / sub colour |
| Ctrl+A / Ctrl+D / Ctrl+Shift+I | Select all / deselect / invert |
| Del | Clear selected area |
| Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y | Undo, redo |
| Ctrl+T / C | Transform / crop; Enter applies, Esc cancels |
| U | Panel frame tool (Enter closes a polygon, Esc cancels) |
| 3D tool: middle drag / Shift / Ctrl, wheel | Orbit / pan / zoom the 3D layer's camera |
| 3D tool: G / R / S, 1 / 3 / 7, 5, Home | Gizmo mode, views, perspective, frame all |
| Ctrl+C / Ctrl+X | Copy / cut the selected pixels of the active layer |
| Ctrl+V / Ctrl+Shift+V | Paste on a new layer, centred in the view / in place |
| Ctrl+N / Ctrl+O / Ctrl+S / Ctrl+Shift+S | New / open / save / save as |

## Credits

Interface icons: GIMP's symbolic icon theme by the GNOME Project and the GIMP
team (https://www.gimp.org/), CC BY-SA 3.0 US / 4.0. See
[crates/painter-app/assets/icons/COPYING](crates/painter-app/assets/icons/COPYING).

## iPad

GitHub Actions builds an unsigned iPad app on every push (no Mac needed); see
[docs/IPAD.md](docs/IPAD.md) for installing it with a free Apple ID. On the iPad the
Apple Pencil draws and fingers navigate (pan, pinch, rotate, two-finger tap to undo).

## Licence

Painter is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option. The GIMP symbolic icons in `crates/painter-app/assets/icons` keep their
own licence (see the `COPYING` file there).
