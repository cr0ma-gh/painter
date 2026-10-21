# Architecture

```
crates/
  painter-core    no GPU, no UI: document, tiles, brush engine, selections, fill,
                  procedural layers, undo, CPU compositing, .painter / PSD / PNG IO
  painter-render  wgpu: sparse tile atlas, WGSL compositor, stroke preview, presentation
  brush-io        brush presets: Clip Studio .sut/.todb (SQLite), Photoshop .abr,
                  native .rustbrush (see BRUSH_IO.md)
  painter-app     eframe/egui shell: docked panels, tools, input, WinTab, file dialogs
```

## Planes and the tile atlas

Everything tiled is a **plane**: a layer's pixels (`Plane::Color`), a layer
mask (`Plane::Mask`, alpha = hidden amount) or the selection (`Plane::Selection`,
alpha = selected).

- **CPU**: each plane is a `TileStore`, a `HashMap<TileCoord, Arc<Tile>>` of
  64×64 premultiplied RGBA8 tiles. Empty tiles are not stored. Undo snapshots
  clone the layer tree (plus the selection), which copies `Arc`s, not pixels.
- **GPU** (`atlas.rs`): every resident tile of every plane lives in one shared
  `texture_2d_array` atlas (2048² pages, 1024 tiles each, grown by doubling).
  Each plane has a small `R32Uint` indirection texture with one texel per canvas
  tile (`0` = empty, otherwise `slot + 1`). Shaders read planes through
  `atlas_load` (`shaders/atlas.wgsl`). GPU memory follows the painted area, not
  the layer count. The fixed cost is the composite targets: two per folder
  nesting depth, plus the stroke and cache textures.

## Data flow for one stroke

```
pointer/pen events ──► TabletState (WinTab pressure/tilt/eraser, or Windows Ink)
        │
        ▼
StrokeEngine: Smoother (average | pulled string) ──► spacing walk ──► Dab{x,y,radius,alpha}
        │
        ├── while drawing (UI thread → GPU, every frame) ──────────────────────────────┐
        │                                                                              ▼
        │                             dab.wgsl: instanced quads ──► stroke texture (Rgba16Float)
        │                                                                              │
        │    stroke_merge.wgsl: merge(plane via atlas, stroke × opacity × selection) ──► preview
        │                                                                              │
        │      composite reads the preview wherever that plane is used (layer colour,
        │      layer mask, or the clipping base of layers above)
        │
        └──► BackgroundRasterizer (worker thread)
               raster::stamp_dab ──► StrokeBuffer (f32 tiles)
                        │ on pen-up: join
                        ▼
               raster::commit_stroke(before, buffer × opacity, mode, alpha-lock, selection)
                        ▼
plane TileStore (source of truth) ──► Upload::Tile ──► atlas   (preview dropped the same frame)
```

Vector layers record the input points, so their stroke can be replayed. The vector
eraser removes whole strokes it touches and re-rasterises the cache. Fill,
gradient and Delete build a `RasterizedStroke` directly and commit it through
the same merge.

### Live preview vs. committed pixels

The CPU tiles are authoritative for undo and export. Both paths use the same
math, with matching pairs:

- `dab.wgsl` ↔ `raster::stamp_dab`
- `stroke_merge.wgsl` ↔ `raster::merge_stroke`
- `composite.wgsl` ↔ `io::blend_source`
- the procedural formulas ↔ `procedural.rs`

`painter-render/tests/gpu_parity.rs` renders headlessly and compares GPU output
against the CPU (at most 3/255 per channel). It covers every brush type, mask
editing, selection-limited strokes, clipping bases, all layer kinds, nested
folders, mid-stroke edits and atlas growth.

## Compositing

`build_plan` turns the visible tree into `PlanNode`s: blend, opacity, mask, clip
base and kind. Each node is one fullscreen pass that computes

```
source × (1 − mask) × clip × opacity
```

and blends it with one of 13 W3C separable modes. Isolated folders render their
children on the next depth's targets, then blend that result as one source.
Compositing reruns only when something changed. Navigation reruns only the
present pass, which also draws the selection's marching ants.

### Stroke-time caching

During a stroke `split_at_layer` splits the stack around the painted layer, with
one level per isolated folder on the path. Per level it caches **below**, and
**above** only when every layer above is Normal and none clips to the painted
layer. Source-over is associative; other modes, and layers clipped to the painted
layer, depend on what changes. A frame with new dabs then runs
`below ⊕ preview ⊕ above` per level. That is 2 blend passes for a plain stack,
whatever its layer count.

## File formats

- **`.painter`** (`project.rs`): a zip with `document.json` (the layer tree,
  every property, vector strokes, brush settings) and one zstd-compressed tile
  blob per plane. Saves go through a temporary file and a rename.
- **PSD** (`psd.rs`): written from the format specification, with PackBits RLE
  channels, `lsct` group dividers, `luni` names and user masks. Reading accepts
  8-bit RGB with raw/RLE channels and rejects other variants with a clear error.
- **Krita `.kra`** (`kra.rs`) and **OpenRaster `.ora`** (`ora.rs`): ZIPs
  through `zipfile.rs`, a small reader / writer (stored and deflated entries)
  that can store the leading `mimetype` uncompressed as both formats require.
  `.ora` has `stack.xml` and one PNG per layer. `.kra` has `maindoc.xml` and
  each layer in Krita's tiled format: 64 × 64 tiles, each LZF-compressed
  planar BGRA (or raw), unpremultiplied; 8 / 16-bit RGBA and grey layers and
  transparency masks (`.pixelselection`) are read. As in Krita's loader, a
  device's `.defaultpixel` fills what its tiles don't cover (transparent, and
  so a fully hidden mask, when absent), and "inherit alpha" (alpha left out of
  `channelflags`) becomes clipping. In `.ora`, a stack without `isolation` is
  isolated (the spec's default) and `svg:src-atop` becomes clipping. Both also carry the
  composite and a thumbnail. `interchange.rs` holds what they share (a layer's
  pixels whatever its kind, blend mode names). Loading returns warnings for
  what was approximated; the app shows them in the status bar.

## Input

`TabletState` polls WinTab once per frame when `wintab32.dll` is present. It
gives pressure, tilt and the active pen end, and flipping the pen to its eraser
end switches tools. Without WinTab it falls back to Windows Ink touch-force
events.

All packets of a frame are kept, and spread over that frame's pointer events
in order. In a fast stroke each sample gets its own pressure and tilt, not the
frame's newest. Tilt feeds the brush dynamics (size, density, opacity,
thickness, rotation).

Known limit: packets and pointer events are matched by order within a frame,
not by timestamp.
