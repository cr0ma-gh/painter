# Brush import / export (`crates/brush-io`, library `brush_io`)

| Module | Format | Read | Write |
|---|---|---|---|
| `sut.rs` | Clip Studio Paint sub tool `.sut`, tool database `.todb` (SQLite) | ✅ | — |
| `abr.rs` | Photoshop `.abr` v1, v2, v6, v7, v10 | ✅ | — |
| `native.rs` | Painter `.rustbrush` (JSON) | ✅ | ✅ |
| `cache.rs` | On-disk cache of imports (as `.rustbrush`) | | |
| `types.rs` | `BrushPreset`, `ImportReport`: the unified representation | | |

All readers produce `BrushPreset`. A preset holds three things:

- the engine's `BrushSettings`, which is what Painter paints with;
- the bitmaps: the sampled `tip` (with `thickness` and `angle` for flat or
  rotated tips), a paper `texture` (luminance, scale %, density, invert) and a
  `dual` tip (scale relative to the main diameter).
  `BrushPreset::apply_tip()` turns them into the engine's `settings.tip`,
  `settings.texture` and `settings.dual`; every reader calls it;
- `dynamics`: pressure, velocity and tilt curves, random jitter, rotation and
  thickness for every parameter. `apply_dynamics()` maps all of them into the
  engine (see "Dynamics" below);
- `extras`: every source parameter without a field, kept verbatim, so a
  `.rustbrush` export loses nothing.

`BrushPreset::unsupported_features()` lists what the engine doesn't render yet.
The UI shows it on hover and in Tool Property.

## Fallbacks and the import report

`import_report()` returns an `ImportReport`: the presets, `warnings` and
`from_cache`. A part that can't be decoded never fails the whole file. Each
case is recorded in `warnings`, which the app lists in the Sub Tool panel:

- an unreadable or missing tip (bad bounds, unknown compression or depth,
  truncated data) becomes a round tip;
- a missing or unreadable texture pattern means no texture;
- a missing dual tip drops the dual brush;
- unreadable brush parameters (`desc`) leave one preset per tip.

The whole file fails only when it isn't the format at all (no `Node`/`Variant`
tables, no `8BPS`-style ABR header, wrong JSON).

## Cache

`cache::import_cached(path, cache::default_dir())` stores each successful
import as `.rustbrush` in `%LOCALAPPDATA%\Painter\brush-cache`. The key covers
the path, size, modification time and a cache version. Imports with warnings
aren't cached, so installing a missing material takes effect on the next import.

On Clip Studio Paint 1.5's 144-sub-tool database the import takes 209 ms parsed
and 27 ms from the cache.

## Clip Studio Paint (`.sut`, `.todb`)

The format was learned from Clip Studio Paint 1.5's own tool database on this
machine, read-only on a copy. Nothing in the application binary was examined.

- **`Node`:** the sub-tool tree. `NodeName` is the sub tool name and
  `NodeVariantID` points to its parameters.
- **`Variant`:** one row per sub tool, with named columns. Brushes are the rows
  with a `BrushSize`. The reader discovers the columns at runtime.

| Clip Studio | Painter |
|---|---|
| `BrushSize` (px) | `size` |
| `Opacity`, `BrushFlow`, `BrushHardness` (%) | `opacity`, `density`, `hardness` |
| `AntiAlias` 0–3 | `AntiAlias::{None, Weak, Medium, Strong}` |
| `FlickerReduction` | `stabilization` |
| `CompositeMode = 27` | eraser |
| `BrushInterval` + `BrushAutoIntervalType` | `spacing`: exact for a fixed interval (type 0). Auto levels are approximated as 1 → 5%, 2 → 10%, 3 → 25% |
| `BrushThickness`, `BrushRotation` | `thickness`, `angle` |
| `Brush{Size,Flow,Opacity}Effector` | `dynamics.*.pressure`. Size and flow drive `size_curve` / `density_curve`; opacity drives per-dab `opacity_dynamics` |
| `BrushThicknessEffector` | kept in `extras` only (see below) |
| `BrushRotationEffector` (integer mask) + `BrushRotationRandomScale` | bit `0x80` → `rotation.jitter` = RandomScale %; bit `0x20` → `rotation.follow_direction` |
| `BrushPatternReverseHorizontal` / `Vertical` = 2 | `rotation.random_flip_x` / `random_flip_y` |
| `BrushUsePatternImage` + `BrushPatternImageArray` | `tip`, from the material thumbnail (see below) |
| `TextureImage`, `TextureScale2` (else `TextureScale`), `TextureDensity`, `TextureReverseDensity` | `texture` (scale %, density, invert), from the material thumbnail |

The rotation and flip mappings are inferred from the stock presets: bit `0x80`
is set exactly on the scatter-style brushes (Noise, Gauze, Rose, Pointillism,
Mechanical pencil…) and is the only case where `BrushRotationRandomScale`
differs from its default of 100; bit `0x20` on the flat ones (Flat marker,
Flat watercolor brush); `Reverse* = 2` on natural and scatter brushes. Other
reverse values (3 on Oil paint) stay in `extras`. Newer databases leave
`TextureScale` NULL and keep the scale in `TextureScale2`.

`BrushThicknessEffector` isn't applied: Mechanical pencil's curve falls from 1
to 0 with pressure, and read as "pressure → thickness" it flattened every firm
dab into a sliver. Clip Studio evidently reads it differently.

### Effector BLOB layout

Verified against the G-pen, Mapping pen, Turnip pen and Calligraphy presets:

- **Header:** 12 big-endian `i32` words. Word 0 is 48 (the header size), word 2
  is the enabled-source mask, and words 8 and 9 are the byte lengths of two
  curve blocks.
- **Curve block:** `[12, n, 16]` followed by n points of big-endian `f64`
  (x, y). Lengths check out exactly (12 + 16n).
- **Bit `0x10`, curve 1:** pen pressure. It's set on G-pen and absent on Calligraphy.
- **Curve 2 and the other bits:** meaning not confirmed. They're kept in
  `extras` and not mapped.

### Tip and texture images

Clip Studio keeps these images as installed materials, not in the tool
database. A brush refers to one as `:PaintNNN:<uuid>:data:material.layer`, and
the material lives under
`%APPDATA%\CELSYSUserData\CELSYS\CLIPStudioCommon\Material\Install\PaintNNN\<uuid>\`.

- **`material.layer` isn't read.** It's a proprietary container (`\x89C2F`
  signature, `HEAD`/`dATA` chunks) whose payload is encrypted. Decrypting it
  would mean circumventing a protection on assets licensed for Clip Studio, so
  Painter doesn't.
- **The plain `thumbnail/thumbnail.png` is used instead** (`MaterialLibrary`).
  As a tip, dark opaque pixels are ink. As a texture, it's luminance, with
  transparent pixels counted as white.
- **This is an approximation.** Thumbnails are about 300 px previews, so very
  large or very detailed tips lose resolution. Presets record
  `extras["sut.tip_source"] = "material thumbnail (approximate)"`.
- **Real data:** on this machine the 144 sub tools gave 75 tips and 26
  textures, with no fallbacks.
- **Embedded PNGs:** a `.sut` export can embed materials in a `MaterialFile`
  table. A single PNG found there is used as the tip. No such file was
  available to test.

## Photoshop (`.abr`)

The layout follows the community documentation implemented by GIMP's and
Krita's loaders.

- **v1 / v2:** computed brushes (diameter, spacing, hardness, roundness, angle)
  and sampled brushes (raw or PackBits tips; v2 adds UCS-2 names).
- **v6+:** `8BIM` sections.
  - `samp`: tips keyed by UUID, 8- or 16-bit, raw or RLE; subversions 1 and 2.
  - `patt`: texture patterns (grayscale, RGB or indexed; raw or RLE channels
    in a "virtual memory array list"), converted to luminance.
  - `desc`: an ActionDescriptor (`descriptor.rs`). It gives:
    - name, `Dmtr`, `Spcn`, `Hrdn`, `Angl`, `Rndn`, `Intr`;
    - `szVr`/`opVr` dynamics: `bVTy = 2` pen pressure, `3` pen tilt, plus
      `minimumDiameter` and jitter;
    - `angleDynamics`: jitter (100% = ±180°); `bVTy = 6/7` (initial)
      direction → the tip follows the stroke, `2`/`3` → a full turn from
      pressure / tilt;
    - `roundnessDynamics` + `minimumRoundness`: tip thickness from pressure
      (`2`) or tilt (`3`), plus jitter;
    - texture: `useTexture`, then `Txtr` → pattern by `Idnt` (or by name),
      `textureScale`, `textureDepth` → density, `InvT` → invert;
    - dual brush: `useDualBrush`, then `dualBrush.Brsh` → a sampled tip, or a
      soft disc for a computed one, scaled by its `Dmtr` over the main one.

    Scatter, texture blend mode and the rest go into `extras`.

The tests use ABR files built to the documented layout. No real `.abr` was
available on this machine to check against.

## Exporting for Photoshop and Clip Studio

- **`.abr` (`abr_write`)**: version 6.2, the layout the reader above takes:
  tips in `samp` (8-bit raw), textures in `patt` (8-bit grayscale pattern),
  and a `desc` with a `brushPreset` per brush (computed or sampled tip,
  shape dynamics with the minimum diameter, angle / roundness dynamics,
  flips, transfer, texture, dual brush). Values come from the engine
  settings, so edits made in Painter are exported. The round-trip test reads
  the result back with `abr::read_report`. Not carried (Photoshop has no
  matching setting, or its descriptor keys aren't confirmed): stabilisation,
  scatter, colour jitter, the starting taper and the exact shape of pressure
  curves. Opening the file in Photoshop itself hasn't been verified here.
- **`.sut` (`sut::write_with_template`)**: Clip Studio's schema isn't
  published and its materials are encrypted, so the export starts from a
  sub tool the user exported from Clip Studio (any brush). The copy keeps
  every table and setting of that template and replaces, on its first brush,
  the name and the columns the reader maps: size, opacity, flow, hardness,
  spacing (manual interval), anti-aliasing, stabilisation, erase mode,
  thickness, angle, rotation-follows-direction / random bits, random flips
  and the size / flow / opacity pressure curves (the minimum size folded
  into the size curve; the effector header words of the template are
  kept). Columns the template's Clip Studio version lacks are reported.
  Tip, texture and second-tip images stay the template's.

## `.rustbrush`

```json
{ "format": "rustbrush", "version": 1, "presets": [
  { "settings": { ... BrushSettings ... },
    "tip": { "width": 64, "height": 64, "png": "<base64 grayscale PNG>" },
    "dynamics": { "size": { "pressure": { "points": [[0,0],[1,1]] } } },
    "thickness": 1.0, "angle": 0.0,
    "source": { "kind": "clip_studio", "variant_id": 12 },
    "extras": { "sut.BrushUseVectorEraser": 1 } } ] }
```

`settings.tip`, `settings.texture` and `settings.dual` aren't written. They're
rebuilt from `tip`, `texture`, `dual`, `thickness` and `angle` on read.
Textures and dual tips are stored like `tip`: `{ "width", "height", "png" }`.

## In the app

- Drop `.sut`, `.abr`, `.rustbrush` or `.todb` files anywhere on the window, or
  use Sub Tool → Import… or File → Import Brushes….
- Passing a brush file on the command line imports it too.
- Imports go through the cache. The status bar says when an import came from
  the cache or needed fallbacks, and the Sub Tool panel lists the fallbacks.
- Imported brushes show their tip thumbnail and paint with their tip, texture
  and dual tip. Hover lists what the engine doesn't apply yet.
- Export the selected brush or all brushes as `.rustbrush`.

Inspect a file from the command line:

```bash
cargo run -p painter-brush-io --example inspect -- brushes.abr
```

Add `--cached` to go through the cache like the app does, and
`--extras sut.TextureScale2,sut.BrushRotationEffector` to print raw source
parameters next to each preset.

Paint a test stroke (an S curve, pressure 0.1 → 1 → 0.1) with one preset:

```bash
cargo run -p painter-brush-io --example render_stroke -- tools.todb "Mechanical pencil" out.png
```

## How tips are painted (`painter_core::tip`)

- **Sampled:** the bitmap is scaled so its longer side spans the dab diameter,
  flattened to `thickness` across its height, rotated by `angle` (degrees,
  counter-clockwise on screen) and sampled bilinearly, zero outside. Hardness
  and anti-aliasing don't apply. Tips much larger than the dab use a
  box-filtered mip chain; the level (finest one whose texels are ≤ 1 canvas
  px) is chosen per dab on the CPU and handed to the GPU, so both agree.
- **Round, `thickness` < 1:** a rotated ellipse with the usual hardness
  falloff. The anti-aliased rim stays `antialias` pixels wide on the thin axis.
- **Round, `thickness` = 1:** the original disc; `angle` has no effect.

- **Dual tip** (`DualTip`): a second bitmap, unrotated and centred on the dab,
  with a diameter of `scale` × the dab's. Its bilinear coverage multiplies the
  dab's. Photoshop scatters dual tips along the stroke with their own spacing;
  Painter approximates that with one dual stamp per dab.
- **Paper texture** (`DabTexture`): a tiling luminance bitmap fixed to the
  canvas, at `scale` canvas px per texel. Coverage is multiplied by
  `1 − density · (1 − t)`, where `t` is the wrapped bilinear sample (inverted
  if `invert`). Light lets ink through; dark holds it back.

### On the GPU

`painter-render`'s `assets.rs` is the texture asset manager.

- **Upload and cache:** each tip, dual tip and texture is uploaded once as an
  `R8Unorm` texture with its full mip chain. Textures are kept by bitmap
  identity and evicted least-recently-used past a 256 MB budget.
- **Filtering in the shader:** `dab.wgsl` filters manually with `textureLoad`,
  clamped for tips and wrapped for textures. Mips stand in for minification.
  The CPU rasteriser does the same, so preview and commit match (`gpu_parity`
  tests, at most 3/255).
- **Vector strokes** keep tip, texture and dual tip in their `BrushSettings`.
  `.painter` files store them as PNG bytes and replay strokes with them.

## Dynamics (`painter_core::stroke`)

`StrokeEngine` computes every dab's radius, alpha and rotation once. The CPU
rasteriser and the GPU preview stamp the same dabs, and vector strokes replay
them exactly.

- **Pressure:** the size and density curves. `opacity_dynamics`,
  `thickness_dynamics` and `rotation` also take a pressure curve.
- **Per-dab parameters:**
  - size, density, opacity (multiplies density per dab) and tip thickness
    (multiplies the tip's own) each have an `InputDynamics`: pressure, speed
    and tilt curves plus jitter, all multiplied;
  - rotation adds each of its pressure, speed and tilt curves × 360°;
  - thickness is per dab, so a round tip becomes a disc or an ellipse dab by
    dab, the same way on the CPU and in `dab.wgsl` (instance data).
- **Velocity:** the speed between smoothed samples, from each
  `StrokePoint.time`, normalised to `FAST_SPEED` (3000 canvas px/s). It feeds
  the optional velocity curve of `size_dynamics` / `density_dynamics`.
- **Tilt:** `StrokePoint.tilt` (WinTab: 0 upright, 1 flat) feeds the optional
  tilt curves.
- **Jitter:** each dab multiplies size and density by a factor uniform in
  `1 − jitter ..= 1`.
- **Rotation:** `rotation.follow_direction` turns the tip with the stroke, and
  `rotation.jitter` adds ±180° × jitter. The angle is per dab: `Dab.angle` is
  added to the tip's own angle, on the CPU and in `dab.wgsl` (instance data).
- **Random flips:** `rotation.random_flip_x` / `random_flip_y` mirror the tip
  on a random half of the dabs. A top-bottom flip is a left-right mirror plus
  a half turn, so each dab carries only `Dab.mirror` (sent to the GPU as the
  sign of the instance's flatness). Without them an irregular tip stamped at
  the same orientation repeats as a visible sawtooth along the stroke edge.
- **Randomness:** an xorshift generator seeded per stroke draws five numbers
  per dab (the flips use the low bits of the rotation draw, so enabling them
  doesn't shift any other dynamic). The app seeds with the stroke id, and `VectorStroke.seed` stores it
  for replay.

The Tool Property panel exposes all of it: speed and tilt responses (off /
less / more) for size, density, opacity and thickness, pressure responses for
opacity and thickness, rotation from pressure/speed/tilt (off / 90° / 360°),
jitter, and following the stroke direction.

Not mapped from Clip Studio: the second curve and other source bits of
effectors (speed or tilt, unconfirmed), the thickness effector, the pressure
and other bits of `BrushRotationEffector`, and texture contrast, brightness
and blend mode. They stay in `extras`.
