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
- **Files**: Painter has its own folder in the Files app (On My iPad →
  Painter). Saving and exporting write there; copy projects, PNG / PSD
  images, `.blend` / `.obj` models and brushes into it and open them from
  **File → Documents**.
- If the app closes unexpectedly, `painter-crash.txt` appears in that folder
  with the reason; please attach it to a bug report.

## Not on iPad (yet)

- WinTab tablets and WIA scanners are Windows-only.
- Settings (workspaces, themes) aren't kept between launches yet.
- The system document picker and drag and drop from other apps aren't wired up.
