# Feature Map

This map describes *behaviour* we want to match, derived from Clip Studio Paint's
public documentation and normal use of the application. No CELSYS code, shaders,
resources, or assets were decompiled or copied.

## Observation notes (REA)

- `CLIPStudioPaint.exe` is a ~35 MB x64 PE. Its PE resource tree contains only
  icons (`RT_ICON`/`RT_GROUP_ICON`), a manifest and version info: no dialogs,
  menus or string tables.
- UI text and layouts ship as GUID-named, per-language data files under
  `resource/<language>/`. We did not parse their contents; nothing from them is used.
- Third-party libraries next to the exe show the general stack: Skia (2D
  rasterization / text), libpng / libjpeg / libtiff / libwebp / giflib (codecs),
  ICU (text), Boost, OpenAL (audio for animation), FBX SDK (3D models), and an
  on-device ML runtime (`ailia*`). Takeaway for us: a CPU raster core plus GPU
  display is a proven split.

## Layers
| Behaviour | Our model | Status |
|---|---|---|
| Raster layers | `LayerKind::Raster(TileStore)`, sparse 64×64 tiles | ✅ |
| Vector layers (strokes kept, whole-stroke vector eraser) | `LayerKind::Vector` (strokes + raster cache) | ✅ |
| Paper layer | `LayerKind::Fill` | ✅ |
| Gradient layers (linear / radial, editable) | `LayerKind::Gradient` | ✅ |
| Tone (screentone) layers, from selection | `LayerKind::Tone` + mask | ✅ |
| Folders, nested; pass-through vs isolated | `LayerKind::Folder` | ✅ |
| 13 blend modes, opacity, visibility, lock, lock transparency | `Layer` fields | ✅ CPU + WGSL |
| Clip to layer below | `Layer::clip`, `clip_targets` | ✅ |
| Layer masks (paint/erase/fill/gradient on the mask, enable/disable) | `Layer::mask` | ✅ |

Rules that differ from Clip Studio Paint in details:

- Clipping uses the base layer's pixel alpha. The base's mask and opacity are not
  applied to the clipped layer. If the base is hidden, the clipped layers are
  hidden too. A base without pixels (paper, gradient, tone, folder) does not clip.
- A pass-through folder that has a mask, or that is clipped, is composited in isolation.
- Vector strokes ignore the selection. Vector layers are rasterised when written to PSD.

## Brush engine
| Parameter | Our field | Status |
|---|---|---|
| Size, min size, pressure curve | `size`, `min_size_ratio`, `size_curve` | ✅ curve editor |
| Opacity (per-stroke ceiling) / density (per-dab) | `opacity`, `density`, `density_curve` | ✅ |
| Spacing, hardness, anti-aliasing (4 levels) | `spacing`, `hardness`, `antialias` | ✅ |
| Stabilization: weighted average / pulled string | `stabilization`, `smoothing` | ✅ |
| Brush tip shape: round, sampled bitmap; thickness and angle (calligraphy) | `tip: DabTip` | ✅ CPU + WGSL, from imported presets (no UI to edit yet) |
| Eraser; auto-switch on the pen's eraser end | `BrushMode::Erase`, WinTab | ✅ |
| Tilt, speed, random jitter | `size_dynamics` / `density_dynamics` (curves + jitter) | ✅ |
| Tip rotation: follow direction, random | `rotation` (per-dab angle) | ✅ |
| Per-dab opacity and tip thickness from pressure, speed, tilt, jitter | `opacity_dynamics`, `thickness_dynamics` | ✅ |
| Rotation from pressure, speed, tilt | `rotation.{pressure,velocity,tilt}` (curve × 360°) | ✅ |

## Selection, fill, gradient
| Behaviour | Status |
|---|---|
| Rectangle, ellipse and lasso selection; new / add (Shift) / subtract (Alt) / intersect | ✅ |
| Select all, deselect, invert; marching ants; painting limited to the selection | ✅ |
| Clear selected area (Delete) | ✅ |
| Fill bucket: tolerance, contiguous / global, refer to all layers, expand N px | ✅ |
| Gradient tool: paint onto a layer or mask, or edit a gradient layer | ✅ |

## Navigation & workspace
| Behaviour | Status |
|---|---|
| Pan, zoom at cursor, rotate, fit; navigator thumbnail with visible-area frame | ✅ |
| Dockable, stackable, floatable panels; layout saved between sessions | ✅ |
| Main / sub colour with swap | ✅ |

## File IO
| Format | Status |
|---|---|
| `.painter` project (everything, including vector strokes, masks and the selection) | ✅ |
| PSD 8-bit RGB read/write: groups, pass-through, blend modes, opacity, visibility, clipping, masks | ✅ |
| PNG open / import as layer / export | ✅ |
| Brush presets: Clip Studio `.sut`/`.todb`, Photoshop `.abr` import; `.rustbrush` import/export | ✅ see [BRUSH_IO.md](BRUSH_IO.md) |
| Rendering imported sampled tips and flat/rotated tips | ✅ |
| Rendering imported tips, textures, dual tips and all dynamics | ✅ (Clip Studio tips/textures from material thumbnails, approximate) |
