# Store listing: Stash Downloads

Copy these into the Chrome Web Store Developer Dashboard and the addons.mozilla.org (AMO) Developer Hub when making the first listing. Keep them in sync with `browser/extension/manifest*.json` and `PRIVACY.md`.

## Name

Stash Downloads

## Short summary

Chrome allows 132 characters (the manifest `description`), AMO 250.

> Send your downloads to Stash: faster multi-connection downloads, pause and resume, and torrents.

## Description

> Stash Downloads hands your browser's downloads to Stash, a download manager for Windows.
>
> Stash downloads each file over several connections at once, so big files finish sooner. You can pause a download and pick it up later, even after closing the browser or restarting your PC. Torrent and magnet links work too. Finished files land in a macOS-style stack on your taskbar, ready to drag where you need them.
>
> - Downloads you start in the browser go to Stash automatically. Turn this off any time from the toolbar button.
> - Right-click a link, image, video or audio and choose **Download with Stash**.
> - Downloads behind a sign-in still work: the extension passes along that site's cookies and the page address, the same as the browser would send.
> - If Stash isn't running, the extension starts it. If Stash isn't installed, the browser downloads as usual.
>
> **Requires the Stash app for Windows**, free from https://github.com/frstycodes/stash/releases (also on the Microsoft Store). The extension talks only to the Stash app on your PC. It has no analytics and sends nothing to us.

## Category

- Chrome Web Store: Tools (or Productivity > Tools)
- AMO: Download Management

## Links

- Homepage: https://github.com/frstycodes/stash
- Support: https://github.com/frstycodes/stash/issues
- Privacy policy: https://github.com/frstycodes/stash/blob/main/PRIVACY.md

## Single purpose (Chrome)

> Hands the browser's downloads to the Stash download manager app installed on the user's PC.

## Permission justifications (Chrome "Privacy practices" tab)

| Permission | Justification |
| --- | --- |
| `downloads` | Notices when a download starts, so it can be handed to the Stash app, then cancels the browser's copy so the file isn't downloaded twice. |
| `nativeMessaging` | The only way the extension talks to Stash: it sends the download's address to the Stash app installed on the same PC (native messaging host `app.stash.downloads`). Nothing is sent over the network by the extension. |
| `cookies` | Reads the cookies for the site a download comes from and passes them to Stash for that download only, so files behind a sign-in download the same as they would in the browser. Cookies are never stored or sent anywhere else. |
| `contextMenus` | Adds **Download with Stash** to the right-click menu on links, images, video and audio. |
| `storage` | Saves the extension's settings (such as whether to hand downloads to Stash) in the browser's local storage. |
| Host permission `<all_urls>` | Downloads can come from any site, and reading a download's cookies with `cookies.getAll` needs host access to that site. The extension doesn't inject scripts or read page content. |
| Remote code | No. All code ships in the package. |

## Data usage (Chrome) / data collection (Firefox)

- **Chrome data usage form**: tick *Authentication information* (cookies) and *Web history* (the download's address and the page it came from), because the extension handles them, and certify that they are used only for the extension's single purpose, never sold, never transferred to third parties, and never used for creditworthiness or lending. They go only to the Stash app on the user's own PC, which sends the cookies and referrer only to the site the file is downloaded from.
- **Firefox**: the manifest declares `data_collection_permissions: { required: ["none"] }`. The extension doesn't transmit data off the device to the developer or anyone else; it hands it to a local app over native messaging.

## Privacy notes (consistent with `PRIVACY.md`)

> Stash Downloads doesn't collect, store or send any personal data to us or anyone else. When it hands a download to the Stash app on your PC, it includes the file's address, the page address and that site's cookies, so the download works the same as it would in the browser. Stash sends them only to that site, and only for that download. The extension's settings are stored in your browser. There are no analytics, crash reports or ads.

## Reviewer notes (both stores)

> The extension needs the Stash desktop app (Windows) to do anything: it is the native messaging host `app.stash.downloads`, which Stash registers for the current user each time it starts. Install it from https://github.com/frstycodes/stash/releases, run it once, then start any download. Without the app, downloads continue in the browser as usual. The source is plain, unminified JavaScript (background.js, popup.js); no build step beyond copying files.

## Images

Icons are in `browser/extension/icons/` (16, 32, 48 and 128 px PNGs). The 128 px one is the store icon for both stores. Chrome wants the artwork at 96x96 inside the 128x128 canvas with transparent padding; check `icon128.png` against that before submitting.

Screenshots and promo tiles are in `browser/store/images/` (24-bit PNG, no alpha), rendered from `browser/store/promo/promo.html` by `promo/render.ps1` (headless Edge + ffmpeg); edit the HTML and re-run to change them. Upload order: `screenshot-1-downloads`, `-2-right-click`, `-3-popup`, `-4-stack`; `promo-small-440x280` and `promo-marquee-1400x560` go in the promo tile slots.

Sizes each store wants:

| Asset | Chrome Web Store | AMO |
| --- | --- | --- |
| Screenshots | At least 1, up to 5: 1280x800 (or 640x400) PNG/JPEG | Optional, up to 10; 1280x800 recommended |
| Small promo tile | 440x280 PNG/JPEG (needed to be featured) | — |
| Marquee promo tile | 1400x560 (optional) | — |

