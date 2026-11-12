# Microsoft Store listing

Paste these into Partner Center when you create or update the submission.

## Product name

Stash: Downloads stack (the reserved name; `packaging/identity.json` must match it exactly)

## Category

Productivity (subcategory: none), or Utilities & tools.

## Short description

A fast download manager with a Mac-style Downloads stack: many connections per file, torrents, and your newest files one hover away.

## Description

Stash downloads files faster by fetching each one over several connections at once, and keeps your newest downloads in a small pile in the corner of the screen.

Paste a link, or a magnet link, or open a .torrent file. Stash splits the download into parts and fetches them side by side. When a fast connection finishes its part, it takes over half of a slow one's. Pause and resume any time, even after a restart. Set a priority, a speed limit, and how many downloads run at once.

When a download starts, its icon drops into the stack in the corner of the screen and the stack shows its progress and speed. Hover over the pile and your files fan out above it with their names, like the Downloads stack on a Mac. Click a file to open it, or drag it straight into another app.

The Downloads window keeps your history, grouped by day. Search it with typos and filters like type:video or site:github.com. Every finished download keeps its link, so you can download it again if the file was moved or deleted. Select several downloads to pause, resume or remove them together.

Stash is light. The Windows compositor runs its animations, and the download engine sleeps when nothing is downloading. Light, dark, or following Windows.

Tip: Windows hides new tray icons behind the arrow next to the clock. Drag the Stash icon onto the taskbar so it's one click away.

## Product features (one per line)

Downloads each file over several connections at once
Pause and resume, even after a restart
Torrents and magnet links, built in
Priorities, a speed limit and how many downloads run at once
Your newest downloads as a pile in the corner of the screen
New downloads drop into the stack and show their progress and speed
Download history grouped by day, with search that forgives typos
Download a file again from its saved link
Select several downloads and act on them together
Watches Downloads plus any other folders you add
Light, dark, or follow Windows
Can start when you sign in

## Search terms

download manager, downloader, torrent, accelerator, downloads, stack, mac, dock

## Screenshots

Upload `1-downloads.png` and `2-folders.png` (1920 x 1080) from this folder. Screenshots of the Downloads window (light and dark) would show the download manager: take them from the app and add them here.

## Privacy policy URL

https://github.com/frstycodes/stash/blob/main/PRIVACY.md

## Website

https://github.com/frstycodes/stash

## Support contact

https://github.com/frstycodes/stash/issues

## Age rating questionnaire

Stash has no user-generated content, no communication between users, no purchases and no location access. It does use the internet: it downloads the files and torrents you ask for, and shares torrent pieces with other people while a torrent downloads (you can turn off seeding afterwards). Answer the internet and file-sharing questions accordingly; it should come out as 3+ or 7+.

## Notes for certification

Stash is a tray app and a download manager. After it starts, it shows a small stack in the bottom-right corner of the screen and an icon in the system tray.

To test the download manager:

1. Right-click the tray icon and choose Open Stash.
2. Select the orange + button.
3. Paste a link, for example `https://proof.ovh.net/files/10Mb.dat`, and select Download.

The download appears in the window and drops into the stack in the corner.

Capabilities:

- **runFullTrust:** Stash is a Win32 desktop app.
- **internetClient:** downloading.
- **internetClientServer** and **privateNetworkClientServer:** BitTorrent accepts connections from other peers while a torrent downloads or seeds.

The Store version is updated by the Store; its built-in updater is off.
