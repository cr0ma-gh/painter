
Without a keyboard, View → On-Screen Modifier Keys shows Ctrl, Shift and Alt
buttons that latch (on by default on iPad).
# Painter

A GPU-accelerated desktop painting app written in Rust (wgpu + egui), inspired
by the workflow of Clip Studio Paint. It is an original implementation; no
CELSYS code or assets are used.

- [docs/FEATURE_MAP.md](docs/FEATURE_MAP.md): the behaviour we target and what is done
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, GPU tile atlas, data flow, file formats
- [docs/BRUSH_IO.md](docs/BRUSH_IO.md): brushes from Clip Studio `.sut`, Photoshop `.abr`, Krita `.kpp` / `.bundle`, MyPaint `.myb`, GIMP `.gbr` / `.gih` / `.vbr`, `.rustbrush`

## Start

A welcome screen at startup shows what Painter does and how to begin (new
canvas, open, new comic page; it can be turned off); Help → About Painter
has the version and links to the website and the source. The icon is an
ensō (`packaging/painter.svg`, a trace of a brush-drawn circle; PNGs for
every platform in `packaging/`, embedded in `painter.exe` by `build.rs`).

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
  clipping to the layer below, and layer masks. The Layer palette works like Clip
  Studio's, with a thumbnail of each layer (small, large or off; the button
  beside the arrows) (double-click a layer to rename it): Ctrl / Shift + click, or the selection checkbox on each row
  (and "Select all" above the list), selects several layers; drag them above,
  below or into a folder; duplicate (Ctrl+J, folders with their contents),
  group into a folder (Ctrl+G), delete, show / hide, lock or change opacity
  and blend mode for all of them at once (right-click menu too). Draft
  layers stay visible while you work and are left out of PNG, PSD, SVG,
  Krita and OpenRaster exports. Layer colour (as in Clip Studio; layer
  properties or right-click) shows a layer's lines in one colour, light blue
  by default, on screen only.
- **Layer Properties** (Window → Panels → Layer Properties, as Clip Studio's
  Layer Property palette), for the active raster or vector layer:
  - Effect buttons: Border effect (an edge of any thickness and colour round
    the shapes, or a watercolour edge with range, transparency, darkness and
    blur), Extract line (the picture's edges and darkest areas as black
    lines), Tone (the layer as halftone dots: frequency in lpi, circle,
    square, diamond, line, cross or noise dots, angle, density from
    brightness or opacity, a set colour or the image's, white paper or
    see-through between dots) and Layer color.
  - Expression color: Color, Gray or Monochrome (painting keeps to greys or
    black and white; Monochrome can hide its black or its white, with a
    threshold).

  Effects are drawn from the pixels, which stay as painted: they show on
  the canvas and in exports, are saved in the project and turn off again.
- **File object layers** (Layer → New File Object Layer, as Clip Studio's
  file objects and Photoshop's linked smart objects): a picture or document
  (PNG, JPEG, GIF, PSD, Krita, OpenRaster, Clip Studio, Painter) linked, not
  copied. The Transform tool moves, scales, turns and distorts it drawn again
  from the file, so it stays sharp; when the file changes the layer follows;
  Update, Open original, Replace file and Rasterize in the Layers panel.
- **Vector layers:** the eraser takes the touched area, whole strokes, or
  (default, as in Clip Studio) only the touched part **up to where other
  lines cross it**. The **Vector Points** tool (O) picks a line and shows its
  control points: drag one to bend the line there, Alt + click removes it,
  drag the line to move it, Delete removes it; width, colour and Simplify in
  Tool Settings.
- **Straight lines:** with any brush (pen, pencil, eraser…) hold Shift and
  click or drag: the line starts where the last stroke ended (Shift-click
  point after point to join them) or where you press, follows the pointer as
  a guide and is drawn on release with the brush's own dynamics; add Ctrl to
  snap its angle to 15°. Shift snaps the Gradient tool's direction too.
- **Smart shape:** draw a line, circle, ellipse, rectangle or triangle and
  keep the pen still for half a second: it turns into the clean shape (shown
  as a guide) and is drawn with the brush on release. Off in Tool Settings.
- **Shape tool** (Shift+U, as Clip Studio's Figure and Photoshop's shapes):
  rectangles (with rounded corners), ellipses, lines and polygons, outlined
  with the current brush, filled with the main colour, or both. Shift makes
  squares and circles (lines in 15° steps), Alt draws from the centre;
  polygons close on the first corner, a double-click or Enter.
- **Text and balloons** (T and Shift+T, as Clip Studio's text and balloon
  tools and Photoshop's type): click to write, or drag a balloon and write in
  it. Any font on the computer (searchable list), size, colour, left / centre
  / right, vertical columns for manga, line and letter spacing, an outline
  round the letters; characters a font lacks (Japanese, Chinese, Korean,
  symbols) come from a system font that has them. Balloons have Clip Studio's
  sub tools and settings: rounded, rough rounded, ellipse, rectangle, polygon,
  thought and flash balloons, the balloon pen (draw the outline by hand), and
  the balloon tail and thought balloon tail tools (drag out of a balloon; a
  balloon can have several, bent with their middle handle). Line and fill in
  the main, sub or an own colour, add to a new or the selected layer,
  roundness of corner, brush size, anti-aliasing, smooth or rough brush
  shape; handles size and turn a selected balloon; Fit to Text. Texts stay
  editable (click one; drag to move it), live on vector layers, are saved in
  the project and export as pixels;
  a font missing on another computer falls back to the built-in one.
- **Clone stamp, pattern stamp, history brush** (S, Shift+S, Y, as in
  Photoshop), painting with the current brush's tip and dynamics: the clone
  stamp copies from where you Alt-clicked (from the layer or all layers;
  aligned or restarting each stroke); the pattern stamp lays a built-in or
  your own pattern (Edit → Define Pattern, kept for next time), in its colours
  or the main colour; the history brush paints back the state picked with the
  brush button in the History panel (the oldest one otherwise), erased pixels
  included.
- **Languages** (View → Language): English and Italian, the system's language
  by default. Translations are gettext `.po` files in `locales/` that any
  `.po` editor (Poedit, Weblate, Crowdin) opens; a translator can try one by
  dropping it in Painter's settings folder, without building the app. See
  `locales/README.md`.
- **Shortcuts:** hold E for the eraser until you let go (a tap switches);
  double-click the main or sub colour chip for a picker (wheel, RGB, hex).
- **Colour modes** (Edit → Mode, as Photoshop's Image → Mode; shown in the
  status bar), in Photoshop's order:
  - Bitmap: the image flattens into one black-and-white layer by 50 %
    threshold, diffusion dither or a halftone screen of any cell size and
    angle, and painting stays black and white.
  - Grayscale: every layer's colours, vector lines, fills, gradients and
    tones turn grey, and painting stays grey.
  - Duotone: the grey printed with one to four inks (monotone, duotone,
    tritone, quadtone), each with its colour, name and curve, from presets
    (sepia, warm red, cyanotype…) or your own; the canvas shows the inks
    and exports carry them, while the layers stay grey and editable.
  - Indexed Color: the image flattens into at most 256 colours, from its own
    colours (adaptive), exact, web, uniform or grey tables, with forced
    black and white or primaries, transparency, and diffusion, pattern or
    noise dither; painting snaps to the table. Color Table shows it: change
    a colour and it changes wherever it's used, switch to grey, spectrum or
    black body tables, load and save Photoshop `.act` tables.
  - RGB Color, CMYK Color (below), Lab Color (Lightness, a and b channels,
    the colour panel shows L a b), Multichannel (cyan, magenta and yellow
    spot channels, layers merged).

  Undo brings the colours back. 8 bits per channel only: 16 and 32 bits are
  in the menu, not yet available.
- **Colour profiles:** New Canvas picks the colour mode (RGB, Grayscale,
  black and white) and the profile (sRGB, Display P3, Adobe RGB (1998), Gray
  Gamma 2.2); Edit → Mode → Assign Profile changes it. The profile is saved
  in the project and embedded in PNG (grey images as grey PNGs) and PSD
  exports; Painter shows the numbers as they are and converts nothing.
- **CMYK** (New Canvas, or Edit → Mode → CMYK Color; through Little CMS):
  pick a press profile (the ones on the computer, such as Adobe's Coated
  FOGRA39, U.S. Web Coated SWOP, Japan Color, or Windows' SWOP, are found
  by themselves; Load Profile adds any .icc), the rendering intent and black
  point compensation. Every colour becomes what that press can print, and
  what you paint after stays printable (only the new pixels are converted,
  so colours don't drift). The colour panel shows the main colour's C M Y K
  and warns, with a click to fix it, when the press can't print it. PSD
  exports are CMYK files with the profile embedded; the project keeps the
  profile. Painter keeps RGB pixels underneath, 8 bits per channel; press
  profiles aren't shipped with Painter (their licences don't allow it).
- **Clock and battery** (View → Clock and Battery): the time and the
  battery's charge in a small chip in the canvas's corner; right-click it to
  hide it.
- **Channels** (Window → Panels → Channels, as in Photoshop): the colour
  mode's own channels — red, green and blue; cyan, magenta, yellow and
  black in CMYK (the press profile's inks, one alone in grey, several
  printed over each other); Lightness, a and b in Lab; the spot channels of
  Multichannel; Gray, Duotone, Index, Bitmap. Show one alone or a few; click a
  channel to paint, fill and stamp only in it (Shift-click for several, Ctrl+2…5 from the keyboard);
  Ctrl-click loads a channel as a selection. Save selections as named
  channels (Select → Save Selection as Channel), kept in the project, and
  load them back (Shift adds, Alt subtracts).
- **Shared sessions** (File → Shared Session, inspired by Drawpile; in the
  app on Windows, macOS, Linux, iPad and Android, no browser): one person
  hosts the open document (a port, an optional password), the others join
  with the host's address and get it in a new tab. Strokes and fills travel
  as strokes to merge, so two people painting in one spot both keep their
  paint; every other change (layers, their settings, filters, texts) travels
  as the tiles and layer tree that changed. Each person sees the others'
  pointers with their names; there's a chat, a list of who's there and
  Resync when something looks out of step. Undo takes back your own changes
  and leaves others' strokes. Over the internet the host's port has to be
  reachable (forwarded on the router, or a VPN such as Tailscale); the
  session is not encrypted.
- **Actions** (Window → Panels → Actions, like Photoshop's actions and Clip
  Studio's auto actions): record the menu and palette commands you run,
  untick or remove steps, play them back; each action is also a command
  ("Play Action: …") for the palette and the toolbar.
- **History** (Window → Panels → History, as in Clip Studio): every undo step
  by name ("G-Pen · “Inks”", "New Layer “Colours”", "Layer Properties"…);
  click one to go back or forward to it; how many steps are kept.
- **Ruler bar** (View → Ruler Bar, Ctrl+R): pixel or millimetre strips along
  the canvas with the pointer marked; drag a guide out of them, drag it back
  to remove it.
- **Brushes:** size in pixels or, as Clip Studio's option, relative to the
  canvas (a share of its longer side, so a brush looks the same on a small
  or a large document and after resizing it), pressure curves for size and
  density (with an editor),
  per-stroke opacity, per-dab density, spacing, hardness, anti-aliasing,
  stabilisation (weighted average or pulled string), erasers. Blending modes
  per brush (as in Clip Studio: Multiply, Screen, Overlay, Add, Difference…
  and Behind, which paints only under what's already there), read from and
  written to Clip Studio `.sut` brushes; the live preview mixes exactly as the
  stroke lands. On iPad the brush preview follows the Apple Pencil hovering
  over the screen (iPad Pro M2 and later), and a trackpad or mouse pointer.
  The crosshair over the brush outline can be turned off (Tool Settings).
  One engine covers what Clip Studio and
  Photoshop brushes do: image tips with thickness, angle and rotation
  dynamics, paper textures, a second tip (2-brush shape / dual brush),
  speed / tilt / random dynamics, spraying (scattering), per-stroke colour
  jitter and a starting taper. **Advanced Tool Settings** (Tool Settings, or
  the command palette) shows every setting by category, like Clip Studio's
  window, and exports the brush as `.rustbrush`, as a Photoshop `.abr` (tips,
  textures, dual brush, main dynamics), as a Clip Studio `.sut` written into
  a sub tool you exported from Clip Studio once (used as a template: its tip
  images stay, since Clip Studio's material files are encrypted), as a Krita
  `.bundle` or `.kpp`, a MyPaint `.myb` or a GIMP `.gbr` / `.vbr`. Krita (`.kpp`,
  `.bundle`), MyPaint and GIMP (`.gbr`, `.gih`, `.vbr`) brushes import too. An
  imported Krita or MyPaint brush exported back to its own format, unedited,
  is the same file byte for byte; edited, it keeps the settings Painter
  doesn't have. Between different programs the engines differ, so brushes
  are mapped as closely as their settings allow (see docs/BRUSH_IO.md).
- **Extended Color Selector** (Window → Extended Color Selector), after
  Krita's: a hue ring around a triangle, square or diamond, a hue / saturation
  wheel with a value bar, a square with a hue bar, or hue × value and hue ×
  saturation rectangles, in HSV, HSL, HSI or HSY′ (the last two stretched to
  the whole gamut). The ☰ menu shows every shape as a thumbnail.
- **Colour:** colour wheel; the transparent colour (the checkered chip beside main / sub: any brush erases with its own tip and dynamics, until a colour is picked); Color Slider (RGB, HSV and CMYK device values);
  Color Set (named sets, GIMP `.gpl` import / export); Color History;
  Intermediate Color (four blended corners); Approximate Color (value /
  saturation around the current colour); Color Mixing (paint, blend, erase
  and pick on a small mixing canvas). Sets and history are kept between
  launches.
- **Panels** float in their own windows (area menu → Float, or Window →
  the panel's name), fold to their header, and each has its own scale
  (area menu, or Ctrl + Alt + wheel over it). Search Layer finds layers by
  name and kind. The tool strip is customisable: right-click it → Customize
  Toolbar to reorder buttons, remove them, or add any command as a quick
  button. As in Photoshop, tools of a family share one button (brushes,
  stamps, fill and gradient, marquees, lassos, auto and object selection,
  text and balloon): it shows the one in hand or the last used; hold it,
  right-click it or click its corner triangle for the others (Group Tools
  in the strip's right-click menu turns this off). The Layers panel is
  compact: one-line rows (opacity and blend only when not 100 % Normal),
  blend and opacity on one row, locks and switches as small toggles.
- **Tools:** rectangle, ellipse and lasso selections and Auto Select (W, a
  magic wand reading the same boundaries as the fill: tolerance, gap closing,
  reference layers), the magnetic lasso (Shift+L: the path snaps to the
  outlines and drops anchors as you go) and object selection (Shift+W: drag
  a rectangle round something and what stands out from the background is
  selected; colour-based, no AI) (new, add, subtract, intersect); fill bucket (tolerance, contiguous, gap closing up to 40 px, reads the edited layer, all layers or
  layers marked as reference, anti-halo expansion under the lines or inset; ~0.2 s on a 600 dpi A4 page);
  gradient tool; clear selected area. Eyedropper (I): the colour shown on the canvas or only the current
  layer's, one pixel or averaged over 3×3 to 11×11; also while Alt is held
  with a brush, fill or gradient, and by resting a finger on the canvas on
  iPad; a loupe shows the new colour over the old one.
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
  vanishing point they head for, plus verticals / horizontals; a perspective
  grid on the ground and walls, as Clip Studio's, with cell size, number of
  cells and a draggable corner; with one point the walls are a corridor's
  sides), 4-point
  curvilinear perspective (strokes bend into arcs through the left / right or
  top / bottom vanishing points) and 5-point fisheye (plus lines out of the
  centre), mirror,
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
  Every area has an editor-type menu (its icon, and the name when there's
  room), stacked tabs (the hidden ones as icons), Split Right / Split Down /
  Close, and borders that drag. The ^ in an area's header folds it to its
  header strip (or to a narrow icon column beside a side-by-side neighbour),
  like a collapsed Clip Studio palette; its neighbours take the space, and a
  click on the strip opens it again. The interface scales as a whole from 50%
  to 300% (the squares icon at the top right, Window → Interface Size),
  like Blender's Resolution Scale. F3 opens a fuzzy command
  palette (menus, tools, brushes, workspaces, themes); Tab or F11 toggles zen
  mode (canvas only; floating panels stay). Window → Theme Editor edits colours, text size and
  rounding with contrast warnings; dark, light and high-contrast presets,
  themes import and export as JSON. The theme follows the system's light or
  dark mode (a dark theme picked is the one for dark mode, a light one for
  light mode); View → Theme Follows the System turns that off. All of it is
  saved between sessions.
- **Reference board** (Reference View, like PureRef): pictures dropped on it,
  pasted (Ctrl+V), added from files, or live pictures of other open
  documents, laid out freely: move, scale from a corner, turn from the handle
  (Shift snaps to 15°), mirror, grey, opacity, stack, lock, duplicate,
  arrange in rows, fit, mirror the view. The pen draws marks over them to
  point things out (a mark drawn on a picture follows it), the eraser rubs
  them out. Own undo, pan and zoom; kept between launches, and saved to or
  opened from `.refboard` files. Layers dropped on a document's picture are
  copied into that document. Animated GIFs (own decoder) and videos (MP4,
  MOV, WebM, MKV, AVI, WMV…: Media Foundation on Windows, `ffmpeg` elsewhere
  when installed) play in a loop with a progress line: Space pauses, ← / →
  step five seconds, Home restarts. Videos are kept by their path.
- **Bars:** View → Tool Strip / Command Bar (or right-click a bar) hides
  either; the command bar under the menu is customisable like the tool strip
  (any command from the palette, drag to reorder, right-click to remove,
  separators). File → Quit (Ctrl+Q). The reference board also takes PSD,
  Krita, OpenRaster, Clip Studio and Painter files, shown as their flattened picture.
- **Recent files:** the welcome screen shows the files opened or saved lately
  as cards with their picture; File → Open Recent lists them too.
- **Screen modes** (View → Screen Mode, F / Shift+F, as in Photoshop):
  standard, full screen with menus, full screen with the canvas only. C
  toggles the transparent colour (as in Clip Studio; crop is Shift+C), X
  swaps main and sub colour.
- **Several documents:** a tab per open document (`*` marks unsaved changes;
  Ctrl+W closes, Ctrl+Tab / Ctrl+Shift+Tab cycle), each with its own zoom,
  rotation, pan and undo history. Files open and save on worker threads.
  Layers, folders, 3D layers and selections copy into
  another document by dragging the layer onto its tab, from the tab menu or
  Layer → Copy Layer to Document, resampled to keep their print size when the
  resolutions differ (Ctrl+C / Ctrl+V across tabs does the same). File → Save
  All / Close All; quitting lists every unsaved document. Only the active
  document is on the GPU.
- **Brush library:** the brush list (built-in and imported brushes, edits,
  order, tips and textures) is saved to `%APPDATA%\Painter\brushes.rustbrush`
  and comes back at the next launch. Right-click a brush to reset it to its
  default settings (built-in brushes), rename (or double-click it), duplicate
  ("… copy") or delete it; a renamed brush stays in its group; Tool Settings
  has a Reset button too. Right-click the import button to reset every
  built-in brush, reset the whole list (with a warning about imported
  brushes), or add
  back deleted default brushes.
- **Tablet:** WinTab on Windows and XInput2 on Linux (pressure, tilt, every
  packet of fast strokes, automatic switch on the eraser end), with a
  fallback to Windows Ink; Apple Pencil on iPad, stylus pressure on Android.
  Tilt shapes the brush: "Follows the pen's lean" turns the tip towards where
  the pen leans, and "Pencil side" lays it down like Clip Studio's pencils (a
  broad band, up to 4× wider, flatter along the lean and barely lighter; on
  by default for the Pencil, a slider for any brush). Tilt spans 0 (upright)
  to 60° of lean, the range tablet pens report; a normal grip (up to about
  36°) draws a plain line, the side comes in past it. The cursor and Tool
  Settings' tip preview show the tip as the pen would stamp it now (shape,
  image tip, pressure, tilt, lean). Ctrl + Alt and a sideways move of the pen
  or mouse resizes the brush.
- **Reset Settings** (Help → Reset Settings): put back the interface
  (workspaces, panels, toolbars, theme, interface size), the tool options,
  the built-in brushes, the colour sets, the recent files list or the update
  checks, each on its own; documents, actions and patterns stay. Starting
  with `painter --reset-settings` resets the interface and tool options
  before the window shows, for when a setting gets in the way.
- **Updates** (Help → Check for Updates; also once a day by itself, which can
  be turned off): the latest GitHub release against this version. On
  Windows, macOS and Linux (AppImage) "Update and restart" downloads this
  platform's build, checks its size and the SHA-256 GitHub lists, puts it in
  place of the running program and starts it once Painter closes (unsaved
  documents are asked about first). On Android the new APK downloads in the
  browser to install; on iPad the release page opens to sideload the new IPA.
  A version can be skipped. Uses the system's own HTTPS (`curl` on the
  desktop, Java on Android, `NSURLSession` on iPadOS).
- **In the file manager** (File → Set Up File Types on Windows and Linux; built
  into the app on macOS and Android): `.painter` projects open in Painter and
  show their picture as the thumbnail (every project carries a 256 px
  `thumbnail.png`), and PSD, Krita, OpenRaster and Clip Studio files list
  Painter under "Open with" without changing their default app. Windows: per
  user (no administrator), an Explorer thumbnail provider (crate
  `painter-shell`, carried inside `painter.exe`); Linux: MIME type, desktop
  entry and a thumbnailer (`painter --thumbnail IN OUT SIZE`); macOS: the
  document type and a Quick Look thumbnail extension; Android: "Open with".
- **Files:** `.painter` projects, PSD import/export, PNG open/import/export,
  File → New from Clipboard (Ctrl+Alt+N) makes a document the size of a
  picture copied in another app (a screenshot, a browser image) or of the
  selection copied in Painter (at its resolution),
  Krita `.kra` and OpenRaster `.ora` (MyPaint, GIMP, Krita…) open, Save As and
  Export: layers, folders (pass-through or not), opacity, blend modes,
  visibility, lock, offsets and Krita transparency masks are read; on save,
  masks and clipping are baked into the pixels and correction layers left
  out (the status bar says what). Krita filter, vector and fill layers come
  in as a hidden "Krita composite" layer so nothing is lost.
  Clip Studio `.clip` files open (raster and paper layers, folders, opacity,
  blend modes, visibility, lock, clipping, offsets, layer masks; canvases in mm
  or cm come in at their pixel size). Vector, text, 3D and video layers aren't
  stored as pixels in a `.clip`, so the file's own preview comes in as a hidden
  "Clip Studio preview" layer and the status bar names what was left out.
  File → Export → Clip Studio writes a `.clip` into a copy of any file saved by
  Clip Studio (chosen once, kept as the template): layers, folders, opacity,
  blend modes, visibility, lock and clipping; masks are applied to the pixels.
  The app asks before closing with unsaved changes.
- **Print** (File → Print, Ctrl+P): paper size and orientation, margins,
  fit to page, actual size at the document's resolution or a percentage, with
  a preview of the sheet and the resolution it prints at. Windows prints
  through its Print dialog (printer, copies, preferences); macOS and Linux
  send a PDF of the page to a CUPS printer; every platform can save the page
  as a PDF (on iPad and Android it then goes to the share sheet, which prints).
- **Transform and crop** (Transform tool / Ctrl+T, Crop tool / C; crate
  `transform-tools`): the layer or selection floats under a gizmo while the
  document stays untouched, with a live GPU preview and a HUD (mode, X/Y,
  W/H %, rotation, interpolation, Apply / Cancel / Reset). With Nearest
  neighbour, "Whole pixels" keeps pixel art crisp: scale snaps to ×2, ×3… or
  ½, ⅓…, rotation to quarter turns and the position to the pixel grid (½ ×1
  ×2 ×3 ×4 buttons). Edit → Image Size resizes the whole document (layers,
  masks, selection, vector strokes) with Nearest neighbour (keeps pixels),
  bilinear, bicubic or Lanczos3, optionally keeping the size on paper. The
  engine:
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
  and `painter-render`'s depth-buffered scene pass): import `.blend`, `.obj`,
  `.fbx` (binary, 7.x), `.gltf` and `.glb` (meshes, node transforms, the file's
  up axis and unit, base colour and base colour image; **rigged models**
  keep their skeleton and skin, open in their bind pose and can be posed:
  drag a joint on the canvas, or click it and move the pen or mouse (click to
  keep, Esc to cancel); IK bends the bones above it to follow (a chain of 2 by
  default), FK turns the joint's own bone; exact angles in the 3D panel's Pose
  section; poses are saved with the project),
  Blender-style navigation (middle drag orbit, Shift pan, wheel / Ctrl zoom,
  1 / 3 / 7 views, 5 perspective, Home frame; a navigation gizmo in the corner
  shows the axes: click one to look along it, drag it to orbit), click to
  pick, G / R / S gizmo,
  and a panel for transforms, colour, FOV, views and up to four lights. A
  `.blend` with a scene camera opens through that camera (position, rotation,
  focal length, sensor fit, lens shift, clipping; the render frame fitted to
  the canvas) and gets a perspective ruler on the vanishing points of the
  scene's X, Y and Z axes, usable from any layer; the 3D panel can go back to
  the Blender camera or rebuild the ruler from the current view. The other way
  round, **Layer → New 3D Layer from Perspective Ruler** turns a drawing's
  1, 2 or 3 vanishing points into a 3D camera (focal length from two
  perpendicular points, image centre from three, eye 1.6 m above the ground).
  Drop model files on the canvas and each one stands on the ground under the
  pointer, at its real size, in the drawing's perspective (on the active 3D
  layer, or a new one made from the ruler).
  **Texture painting** (3D panel → Texture painting): each object can have a
  colour texture (new, loaded from an image, or an OBJ material's `map_Kd`),
  saved with the project and exportable as PNG. *Paint on model* puts a
  “Paint on model” layer over the 3D layer where every tool works; after each
  stroke its paint is projected onto the texture through the camera (only
  surfaces in view, fading on those seen edge-on, no cracks at UV seams), and
  middle drag or the wheel still turn the model. *Edit texture (UV)* opens the
  texture as its own document with the mesh's UV layout over it and a live
  3D preview; the model follows as you paint. Undo covers both. The
  render is copied into the layer's pixels, so it saves and exports; the scene
  (textures included) is saved in `.painter` files. The engine: `.blend`
  reading (Blender 3.4 – 5.x, plain or zstd-compressed: blocks, the SDNA
  struct catalogue, mesh objects with UVs and transforms), an OBJ reader,
  GPU-ready meshes, a viewport camera with Blender-style navigation (orbit
  turntable / trackball, pan, zoom, dolly, roll, view presets, perspective /
  orthographic, framing), picking and a translate / rotate / scale gizmo with
  axis and plane constraints, trackball and snapping. Written from the file
  format's description and Blender's behaviour, not its GPL source.
  `cargo run -p painter-scene-3d --example preview -- model.blend out.png`
  renders a file on the CPU; `--example probe` lists its objects and structs.
- **Scanner and camera import** (File → Import → From Scanner or Camera, crate
  `scanner-io`): device list, resolution, colour mode, preview with crop,
  auto-contrast and line-art extraction (paper → transparency) into a new
  layer. Works through WIA on Windows (enumeration, capabilities and transfer;
  scans run on a background thread) and through eSCL ("AirScan") with network
  scanners and multifunction printers on every platform, iPad included: found
  on the network by themselves (mDNS on computers, Bonjour on iPad) or by
  typing their address; on Linux, USB scanners through SANE (libsane). TWAIN is still a stub. On iPad
  the document camera (VisionKit) finds the sheet, straightens it and takes
  any number of pages; photos or scans as image files work everywhere. Pages
  get their resolution from the paper size picked (A4, A5, A3, B5, B4, Letter,
  postcard), share the crop and cleanup, and import one by one or all at once
  (a layer each). Each scan opens as a new document at its own size and
  resolution (as Photoshop opens scans), or goes on a layer here: fitted into
  the canvas (scaled down, centred; the dialog says beforehand how much
  resolution that loses), enlarging the canvas, or cut, as chosen; nothing is
  ever kept past the edge of the canvas. "Extract panels" finds the rectangles drawn as
  frames on the page (closed areas of paper surrounded by a frame line, so
  shapes inside drawings don't count) and imports each panel as its own
  layer, where it sits on the page, in reading order (or right to left for
  manga), in a "Panels" folder. `cargo run -p painter-scanner-io
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
| `/`; Ctrl + Space + click, Alt + Space + click | Zoom tool (click in, Alt + click out, drag right / left); zoom in / out for a moment |
| R; Shift + Space + drag, Shift + wheel | Rotate tool (drag round the centre); rotate for a moment |
| H, R, `/` held | The tool only while the key is held, then back (as E) |
| Wheel, Ctrl + wheel | Zoom at cursor |
| Ctrl + NumPad+ / Ctrl + NumPad− | Zoom in / out a step |
| `-` / `^` | Rotate −5° / +5° |
| Ctrl + 0 | Fit to window |
| `[` / `]` | Brush size |
| X | Swap main / sub colour |
| Ctrl+A / Ctrl+D / Ctrl+Shift+I | Select all / deselect / invert |
| Del | Clear selected area |
| Ctrl+Z, Ctrl+Shift+Z / Ctrl+Y | Undo, redo |
| Ctrl+T / C | Transform / crop; Enter applies, Esc cancels |
| U / Shift+U | Panel frame tool (Enter closes a polygon, Esc cancels) / shapes |
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

## Windows installer

`Painter-Setup-windows-x86_64.exe` (crate `painter-installer`, in Rust with
egui) installs Painter for the user alone, without administrator rights: a
welcome page and a tour of the app in slides, what Painter is and its
licence, then the folder (`%LOCALAPPDATA%\Programs\Painter` by default),
Start menu and desktop shortcuts and the file types, and it starts Painter
when it's done. When the computer has them, it also brings in Photoshop's
brushes (`.abr` in Photoshop's presets) and Clip Studio's sub tools (its tool
database and `.sut` files), and can copy Clip Studio's materials folder into
Painter's settings so brushes keep their tips and textures without Clip
Studio (`painter --import-brushes FILE…`, `painter --copy-csp-materials`).
It speaks English or Italian, as Windows does. Painter then
shows in Settings → Apps, whose Uninstall removes it (the settings too, if
asked). `Painter-windows-x86_64.exe` is still there for running without
installing.

Building it: build `painter-app` in release first, then
`PAINTER_EXE=target\release\painter.exe cargo build --release -p
painter-installer` (without `PAINTER_EXE` it builds with no app inside).

## Linux, macOS and Android

GitHub Actions also builds, on every push (download them from the run's
artifacts):

- **Linux**: `Painter-x86_64.AppImage` (`packaging/linux/build-appimage.sh`).
  Make it executable and run it; it uses the system's GTK 3 for file dialogs
  and its Vulkan or OpenGL drivers. Building from source needs
  `libgtk-3-dev libxkbcommon-dev libwayland-dev libx11-dev`. Pen tablets give
  pressure and tilt through XInput2: Painter starts on X11 (XWayland under
  Wayland) for that; `PAINTER_WAYLAND=1` runs it on Wayland directly. USB
  scanners work through SANE (install `sane` / `libsane1`).
- **macOS**: `Painter.dmg` with a universal `Painter.app` (Apple Silicon and
  Intel, macOS 11+; `packaging/macos/build-app.sh`). It's signed ad hoc, not
  notarised: the first time, right-click the app → Open.
- **Android**: `Painter.apk` for arm64 phones and tablets (Android 9+;
  `packaging/android/build.sh`, with cargo-apk), signed with a test key:
  allow installing unknown apps, or `adb install Painter.apk`. Documents live
  in the app's own folder (`Android/data/io.github.cr0ma_gh.painter/files`,
  reachable over USB). File → Open browses it and, once you allow access to
  the device's files, Download, Documents, Pictures and the camera folder;
  exports also get a copy in Documents/Painter. Stylus pressure works, the
  soft keyboard comes up in text fields, network scanners (AirScan) are
  found, pictures copied in other apps paste in, and "Scan with camera" opens
  the camera, then picks the new photo.

## Licence

Painter is licensed under either of [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE),
at your option. The GIMP symbolic icons in `crates/painter-app/assets/icons` keep their
own licence (see the `COPYING` file there).
