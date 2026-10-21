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
  undoes and a three-finger tap redoes.
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
- **Settings** (workspaces and their layouts, zen mode, themes) are saved
  as you change them and come back at the next launch.
- If the app closes unexpectedly, `painter-crash.txt` appears in that folder
  with the reason; please attach it to a bug report.

## Not on iPad (yet)

- WinTab tablets and WIA scanners are Windows-only.
- Drag and drop from other apps (Split View) isn't wired up.
