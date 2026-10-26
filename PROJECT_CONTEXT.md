# PROJECT CONTEXT & AGENT ARCHITECTURE GUIDELINES
> **Repository**: `painter` (Desktop Digital Painting Application)  
> **Tech Stack**: Rust (2021 Edition), `wgpu` 24, `eframe` 0.31 (`egui`), `bytemuck`, `image`, `rusqlite`  
> **Inspiration**: Clip Studio Paint, Photoshop, Procreate  

---

## 1. Project Vision & Architecture

`painter` is a professional-grade, multi-platform desktop digital painting software designed for ultra-low latency stylus input, large canvas dimensions (up to 16k×16k), and natural media simulation.

The codebase is organized as a modular Rust workspace:

```
crates/
├── painter-core        # Pure logic, zero GPU/UI dependencies:
│                       # Document model, 64x64 TileStore, stroke dab synthesis,
│                       # selections, procedural layers, undo tree (Arc snapshots),
│                       # CPU compositing, .painter / PSD / PNG / SVG serialization.
├── painter-render      # wgpu 24 GPU acceleration:
│                       # Sparse tile atlas (2048² pages, R32Uint indirection table),
│                       # WGSL shaders (atlas.wgsl, dab.wgsl, stroke_merge.wgsl, composite.wgsl),
│                       # live stroke preview, presentation pass, marching ants selection shader.
├── painter-app         # Desktop GUI shell:
│                       # eframe/egui interface, dockable panel system, tool controllers,
│                       # WinTab and Windows Ink tablet driver bridges, shortcut maps.
├── brush-io            # Brush engine & preset interchange:
│                       # Native .rustbrush, Photoshop .abr parser, Clip Studio .sut/.todb (SQLite).
├── transform-tools     # Interactive affine transforms, mesh warp, 4-point perspective warp.
├── comic-layout        # Comic strip borders, panel clipping, speech balloon vector meshes.
├── scene-3d            # 3D poseable model reference layer via dedicated wgpu sub-pipeline.
├── vector-export       # Stroke re-rasterizer, centerline vector extraction, SVG export.
├── scanner-io          # TWAIN/WIA high-DPI lineart scan ingestion and binarization.
├── ui-workspace        # User workspace serialization, custom docking configurations.
├── painter-shell       # Windows Explorer thumbnail and icon shell extension DLL.
└── painter-installer   # Lightweight standalone Windows installer binary.
```

---

## 2. Fundamental Architectural Invariants

Every autonomous agent modifying this codebase **must adhere to these invariants**:

### A. The Plane & Tile Atlas Model
1. **Planes**: All canvas data is represented as a `Plane`:
   - `Plane::Color`: premultiplied RGBA8 layer pixels.
   - `Plane::Mask`: single-channel alpha mask (`0` = transparent/hidden, `255` = opaque).
   - `Plane::Selection`: 8-bit selection mask (`255` = fully selected).
2. **CPU TileStore**: Stored as `HashMap<TileCoord, Arc<Tile>>` where each `Tile` is a 64×64 premultiplied RGBA8 buffer.
   - **Never store blank tiles**: Empty space consumes zero heap memory.
   - **Undo mechanism**: Snapshotting clones the tree of `Arc<Tile>`, performing copy-on-write only on modified tiles.
3. **GPU Atlas (`painter-render/src/atlas.rs`)**:
   - Packed into a 2D Array Texture of 2048×2048 pages (1024 64×64 tiles per page).
   - Plane lookup is directed via an `R32Uint` indirection texture (`0` = unallocated, `slot + 1` = atlas index).
   - Shaders access tiles using `atlas_load` defined in `shaders/atlas.wgsl`.

### B. Mathematical Parity (CPU vs. GPU)
The live canvas preview runs on the GPU for instant 60–120 FPS visual feedback, while the committed undo-able state is finalized by `BackgroundRasterizer` on the CPU:
- `dab.wgsl` **must match** `crates/painter-core/src/raster/stamp_dab.rs`
- `stroke_merge.wgsl` **must match** `crates/painter-core/src/raster/merge_stroke.rs`
- `composite.wgsl` **must match** `crates/painter-core/src/io/blend_source.rs`
- Parity is strictly enforced by `crates/painter-render/tests/gpu_parity.rs` with a maximum allowed delta of $\le 3/255$ per channel across all 13 W3C blend modes.

### C. Threading & Latency Guarantees
- The UI thread (`painter-app`) must never block on disk I/O, heavy filter convolutions, or tile rasterization.
- Stylus pointer events generate `Dab` coordinates via the `StrokeEngine` smoother, dispatch immediate GPU draw calls, and asynchronously stream point packets to the worker pool.

---

## 3. Strict Rules of Autonomous Agent Execution

When working as an autonomous coding agent (`claude` / `antigravity_runner`):

1. **Scope Containment**:
   - Modify only the files directly pertinent to the assigned issue.
   - Do NOT reformat untouched code, change dependency versions without explicit instruction, or introduce unrelated refactorings.
2. **Preserve Public APIs**:
   - Maintain backwards compatibility for public structs, traits, and exported functions across workspace crates.
3. **Documentation Integrity**:
   - Preserve existing docstrings and comments. Add clear Rustdoc comments (`///`) to any new struct, method, or enum variant.
4. **Mandatory Verification Gate**:
   Before declaring any task completed, you must execute and pass the local verification gate:
   ```bash
   # 1. Format check
   cargo fmt --check

   # 2. Workspace linting
   cargo clippy --workspace --all-targets -- -D warnings

   # 3. Full test suite execution
   cargo test --workspace
   ```
   If GPU shaders or blending was modified:
   ```bash
   cargo test -p painter-render --test gpu_parity
   ```
5. **Conventional Commits Standard**:
   All commits produced by the agent must strictly follow the Conventional Commits format:
   - `feat(scope): brief description`
   - `fix(scope): brief description`
   - `perf(scope): optimization explanation`
   - `test(scope): test coverage additions`
   - Valid scopes: `core`, `render`, `app`, `brush`, `transform`, `comic`, `scene3d`, `export`, `shaders`, `ci`.
