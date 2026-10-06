<p align="center">
  <img src="assets/icon.png" width="128" alt="Stash icon">
</p>

<h1 align="center">Stash</h1>

<p align="center">A macOS-style Downloads stack for the Windows desktop.</p>

<p align="center">
  <a href="../../releases/latest"><img src="https://img.shields.io/github/v/release/frstycodes/stash?label=download" alt="Latest release"></a>
  <a href="../../actions/workflows/ci.yml"><img src="https://github.com/frstycodes/stash/actions/workflows/ci.yml/badge.svg" alt="Build"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-blue" alt="Windows 10 and 11">
</p>

<p align="center">
  <img src="docs/demo.gif" width="360" alt="Stash fanning out, switching from Downloads to Screenshots and back, then collapsing">
</p>

---

Stash puts a small stack in the corner of your screen showing your newest downloads. Hover over it and it fans out into a list, the way a Dock stack does on a Mac. Click a file to open it, drag it straight into another app, or flick between folders with the scroll wheel.

It's a small native app written in Rust. Animations run in the Windows compositor, so they stay smooth and Stash sits idle when you're not using it.

## Screenshots

<p align="center">
  <img src="docs/stack-closed.png" height="220" alt="The closed stack: a small pile of the newest files in the screen corner">
  &nbsp;&nbsp;
  <img src="docs/stack-open.png" height="520" alt="The open stack: files fanned out along a curve with their names, and Open Downloads at the bottom">
</p>

## Features

- **A live pile of your newest files** in the bottom-right (or bottom-left) corner, using real thumbnails for photos, videos and documents.
- **Fans out on hover.** Every file rises along a gentle curve with its name beside it, and the bottom spot shows which folder you're looking at.
- **Open, drag or tidy up.** Click to open a file, drag it into any app, or right-click to show it in Explorer or move it to the Recycle Bin.
- **Several folders.** Watch Downloads plus any other folders (Screenshots, Desktop, a project folder…) and switch between them by scrolling over the stack.
- **Notices new files.** The stack bounces when a download finishes, even in a folder you aren't viewing.
- **Drop files onto it** to move them into the folder on show.
- **Opens from the tray** on top of whatever you're doing, and fades back behind your windows when you're done.
- **Your choices:** sort order, how many files to show, stack or folder look, icon size (Small to Extra Large), corner, and start at sign-in.

## Install

1. Download `Stash-Setup-<version>.exe` from the [latest release](../../releases/latest).
2. Run it. It installs just for you (no admin prompt) and can start Stash when you sign in.

Prefer not to install? Each release also has `Stash-<version>-portable.exe`, which runs as is.

The installer isn't code-signed yet, so Windows SmartScreen may warn about an unrecognised app. Choose **More info → Run anyway**, or build it yourself from source (below).

Uninstall it from **Settings → Apps → Installed apps**. Your settings stay in `%APPDATA%\Stash`.

**Requires** Windows 10 (version 1903 or later) or Windows 11.

## Using Stash

| To… | Do this |
| --- | --- |
| Open the stack | Hover over it, click it, or left-click the tray icon |
| Open a file | Click it |
| Use a file in another app | Drag it out of the stack |
| Open the folder in Explorer | Click the folder at the bottom of the open stack |
| Switch folders | Scroll over the stack (or pick one under **Folders** in the right-click menu) |
| Add or remove a folder | Right-click the stack or tray icon → **Folders** |
| Change settings | Right-click the stack or the tray icon |

## Build from source

You need [Rust](https://rustup.rs) (stable, MSVC toolchain) and the Windows SDK, which comes with the Visual Studio C++ build tools.

```powershell
git clone https://github.com/frstycodes/stash.git
cd stash
cargo build --release
.\target\release\stash.exe
```

To build the installer as well, install [NSIS 3](https://nsis.sourceforge.io) (`winget install NSIS.NSIS`) and run:

```powershell
.\installer\build.ps1
```

It writes `dist\Stash-Setup-<version>.exe`.

### Debugging

- `STASH_LOG=1` writes a log to `%TEMP%\stash.log`.
- `STASH_SLOW=5` plays every animation five times slower.
- `STASH_PIN_OPEN=1` keeps the stack open so you can inspect it.
- `STASH_DEMO=<folder>;<folder>` shows those folders instead of yours, above every window, and saves nothing. The README screenshots use it.
- `STASH_DEMO_SCRIPT="1.5:open;4:next;6:close"` plays a timed sequence in demo mode (seconds from start; commands are `open`, `close`, `next` and `prev`). The demo GIF uses it.

## Releases

Releases are built and published by GitHub Actions; nobody builds them by hand.

- **Every push and pull request** builds the app and the installer ([CI](.github/workflows/ci.yml)), so a broken build shows up straight away.
- **Pushing a version tag** such as `v0.3.0` builds the installer and a portable exe and publishes them as a GitHub release with notes generated from the commits ([Release](.github/workflows/release.yml)).

To cut a release, run this from a clean `main`:

```powershell
.installerelease.ps1 0.3.0
```

It sets the version in `Cargo.toml`, commits, tags `v0.3.0` and pushes. The release appears on the [Releases page](../../releases) a few minutes later.

## How it works

- **Windows Composition (the Visual Layer).** Every icon in the fan follows a single `Progress` value that the compositor animates with springs, so motion keeps going smoothly even when Stash is busy.
- **Direct2D and DirectWrite** draw the thumbnails, labels and badges.
- **The Windows shell** provides thumbnails (`IShellItemImageFactory`), drag-and-drop and file actions.
- **Folders are watched with `ReadDirectoryChangesW`**, through the [`notify`](https://crates.io/crates/notify) crate, so there's no polling.

## License

Stash is **source-available** under the [PolyForm Noncommercial License 1.0.0](LICENSE.md).

You're welcome to read the code, use Stash, change it and share your changes, as long as it's **not for a commercial purpose**. Personal use, study, hobby projects, and use by charities, schools and public institutions are all fine. For commercial use, please [open an issue](https://github.com/frstycodes/stash/issues) to get in touch.

Copyright 2026 Sandesh Pandey.
