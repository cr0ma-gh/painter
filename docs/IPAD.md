# Painter on iPad

The iPad build is made by GitHub Actions on a macOS runner (`.github/workflows/ci.yml`,
job **iPad build**), so no Mac is needed. It produces an **unsigned** `.ipa`;
iPadOS only runs signed apps, so it has to be signed when it's installed.

## Install with a free Apple ID (Sideloadly, from Windows)

1. Open the repository on GitHub → **Actions** → the latest successful **CI**
   run → download the artifact **painter-ipad-unsigned** and unzip it to get
   `Painter-unsigned.ipa`.
2. Install **iTunes** (from apple.com, not the Microsoft Store version) and
   **Sideloadly** (sideloadly.io).
3. Connect the iPad with a cable and trust the computer.
4. Drag the `.ipa` into Sideloadly, enter your Apple ID, press **Start**.
5. On the iPad: **Settings → General → VPN & Device Management** → trust your
   Apple ID. On iPadOS 16 and later also turn on **Settings → Privacy &
   Security → Developer Mode** and restart.

With a free Apple ID the signature lasts **7 days**; reinstall (Sideloadly can
refresh it) to keep using it. A paid Apple Developer account (99 $/year) gives
one-year signatures and TestFlight.

## Using it

- **Apple Pencil draws, fingers navigate** (View → Draw With the Pen Only):
  one finger pans, two fingers pan, pinch-zoom and rotate, a two-finger tap
  undoes, a three-finger tap redoes and a four-finger tap toggles zen mode
  (canvas only). In zen mode the button in the top-right corner also brings
  the panels back.
- **Files**: File → Open (and every import: images, brushes, 3D models,
  themes) shows the system document picker, so files can come from Files,
  iCloud Drive or other apps. Painter keeps its own folder in the Files app
  (On My iPad → Painter): saving writes there, projects opened from elsewhere
  are copied there first so your changes are kept, and **File → Documents**
  lists what it holds. After an export (PNG, PSD, SVG, brushes, theme) a
  second picker offers to put a copy anywhere else; cancel it to keep just the
  one in the Painter folder.
- **Brushes with textures**: Photoshop `.abr` and Painter `.rustbrush` files
  carry their textures. Clip Studio `.sut` files don't: they point to
  materials installed by Clip Studio, which the iPad doesn't have. Either
  - import the `.sut` in Painter on Windows and use **Export brushes** to get a
    `.rustbrush` with the textures inside, then import that on the iPad, or
  - copy Clip Studio's material folder from the PC
    (`%APPDATA%\CELSYSUserData\CELSYS\CLIPStudioCommon\Material`, or just the
    `PaintNNN` folders inside `Install`) into **Painter → Materials** in the
    Files app, then import the `.sut` again.
- **On-screen Ctrl, Shift and Alt** (bottom left; View → On-Screen Modifier
  Keys): tap a key to hold it, tap again to release it. Every shortcut and
  tool sees them like real keys (Ctrl latched + the Z of a keyboard = undo,
  Shift for straight lines, …). Drag the strip by its dots to move it.
- **Your brushes** (imported ones, edits, order) are kept between launches,
  textures included. Right-click the import button in the brush list to add
  back default brushes you deleted.
- Panels fold with the ^ in their header to free space; tap the strip to
  open them again. The squares icon at the top right sets the interface
  size (it starts at 125% on iPad).
- **Settings** (workspaces and their layouts, zen mode, themes) are saved
  as you change them and come back at the next launch.
- The iOS status bar is hidden and swipes from the screen edges go to
  Painter first, so the menu bar at the top can be tapped; swipe a second
  time to open the Notification or Control Center.
- **New from Clipboard** (File menu): a document the size of a picture
  copied in another app; pasting pictures on the reference board works too.
- **Network scanners:** File → Import → From Scanner or Camera lists the
  scanners and multifunction printers on your Wi-Fi that scan over AirScan
  (eSCL; most made since about 2014). The first time, iPadOS asks to let
  Painter find devices on the local network: allow it. If yours doesn't
  show up, type its address under "Network scanner by address".
- **Save As:** asks for the name and the format (Painter, Krita, OpenRaster)
  and saves in the Painter folder; "Save and copy to..." then lets you put a
  copy anywhere (iCloud Drive, another folder or app).
- **Scanning:** File → Import → From Scanner or Camera → Scan with camera
  opens the iPad's document camera: it finds the sheet and straightens it,
  and you can take several pages. Pick the paper size (it gives the pages
  their resolution), crop, rotate, clean up (auto contrast, line art) and
  import a page or all of them, a layer each. "From photo or file" takes a
  photo or scan from the Files app the same way.
- If the app closes unexpectedly, `painter-crash.txt` appears in that folder
  with the reason; please attach it to a bug report.

## Not on iPad (yet)

- WinTab tablets and USB scanners are Windows-only; on iPad network scanners
  (AirScan) and the camera scan instead.
- Drag and drop from other apps (Split View) isn't wired up.
