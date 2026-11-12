# Stash browser extensions

The extensions send your browser's downloads to Stash, so they get multiple connections, pause and resume, and show up in the stack. They pass along the page's cookies and referrer, so downloads that need you to be signed in still work. Right-click any link to choose **Download with Stash**.

If Stash isn't running, the extension starts it. If Stash isn't installed, the browser downloads as usual.

## Install

Stash registers itself with Chrome, Edge, Brave and Firefox each time it starts, so run Stash once first.

Build the extensions:

```powershell
.\browser\build.ps1
```

- **Chrome, Edge, Brave**: open `chrome://extensions` (or `edge://extensions`), turn on Developer mode, select **Load unpacked**, and pick `browser\dist\chrome`.
- **Firefox**: open `about:debugging`, select **This Firefox**, then **Load Temporary Add-on**, and pick `browser\dist\firefox\manifest.json`. A permanent install needs the add-on signed by addons.mozilla.org (`browser\dist\stash-firefox.zip`).

## How it works

Stash's own exe (`StashDownloads.exe` when installed, `stash.exe` when built from source) is the extension's native messaging host. The browser starts it with the extension's origin, and the extension sends it one length-prefixed JSON message per download:

```json
{ "type": "download", "download": { "url": "…", "filename": "…", "referrer": "…", "cookies": "…", "userAgent": "…" } }
```

The host passes the download to the running Stash and answers `{ "ok": true }`. The extension then cancels the browser's copy. `{ "type": "ping" }` checks the connection, and `{ "type": "show" }` opens the downloads window.

The host manifests live in `%APPDATA%\Stash` (`stash.native.json` and `stash.firefox.native.json`). The registry keys that point to them are under `HKCU\Software\<browser>\NativeMessagingHosts\app.stash.downloads`.

The unpacked Chrome extension's id, `nophahmheeiolmgdgedfjemnmcpdglad`, comes from the `key` in `manifest.json`. A Chrome Web Store listing gets a different id, which the host accepts when the app is built with `STASH_CHROME_STORE_ID` set (see [Publishing](#publishing)).

## Publishing

`build.ps1` sets the extension version in both manifests from `Cargo.toml`, so each release ships extensions with the app's version. It writes three zips to `browser\dist`:

- `stash-chrome.zip`: `dist\chrome` as is, with the manifest `key` (attached to GitHub releases for loading unpacked).
- `stash-chrome-store.zip`: the same without the `key`, which the Chrome Web Store rejects. This is the one to upload.
- `stash-firefox.zip`: for addons.mozilla.org (AMO).

Store text, permission justifications, privacy answers and the images still needed are in [store/listing.md](store/listing.md).

The [Release workflow](../.github/workflows/release.yml) publishes both after the GitHub release. Each store step is skipped, not failed, until its secrets exist, and each store needs one listing made by hand first.

### Chrome Web Store

1. **Developer account**: sign up at the [Chrome Web Store Developer Dashboard](https://chrome.google.com/webstore/devconsole) (one-time $5 fee) and verify the contact email.
2. **First submission, by hand**: run `.\browser\build.ps1`, then in the dashboard select **New item** and upload `browser\dist\stash-chrome-store.zip`. Fill in the store listing, privacy practices (single purpose, permission justifications, data usage) and distribution from [store/listing.md](store/listing.md), add the icon and screenshots, and submit for review.
3. **Ids**: the item's 32-character id is on its **Package** tab (it's also the store extension's id in `chrome://extensions`). Your publisher id is under **Publisher > Settings** (the API v2 needs it).
4. **OAuth client and refresh token** for the [Chrome Web Store API](https://developer.chrome.com/docs/webstore/using-api):
   1. In the [Google Cloud console](https://console.cloud.google.com/), create a project and enable the **Chrome Web Store API**.
   2. Under **Google Auth Platform**, set up the consent screen (External), add yourself as a test user, then **publish the app** to production. A consent screen left in Testing issues refresh tokens that expire after 7 days.
   3. Create an **OAuth client ID** of type **Desktop app**. Note its client id and secret.
   4. Get a refresh token with the scope `https://www.googleapis.com/auth/chromewebstore`. The easy way is `powershell -ExecutionPolicy Bypass -File browser\store\get-chrome-refresh-token.ps1`: it asks for the client id and secret, opens the consent page, catches the redirect on `localhost:8818` and prints the token. By hand: in a browser, sign in with the account that owns the dashboard item and open:

      ```
      https://accounts.google.com/o/oauth2/auth?response_type=code&scope=https://www.googleapis.com/auth/chromewebstore&access_type=offline&prompt=consent&redirect_uri=http://localhost:8818&client_id=CLIENT_ID
      ```

      Approve, then copy the `code` parameter from the address the browser lands on (the page itself fails to load; that's expected). Exchange it:

      ```powershell
      Invoke-RestMethod -Method Post -Uri https://oauth2.googleapis.com/token -Body @{
        client_id = 'CLIENT_ID'; client_secret = 'CLIENT_SECRET'; code = 'CODE'
        grant_type = 'authorization_code'; redirect_uri = 'http://localhost:8818' }
      ```

      The `refresh_token` in the answer is the secret.
5. **Repository secrets** (Settings > Secrets and variables > Actions): `CHROME_PUBLISHER_ID`, `CHROME_EXTENSION_ID`, `CHROME_CLIENT_ID`, `CHROME_CLIENT_SECRET`, `CHROME_REFRESH_TOKEN`.
6. **Let the app accept the store extension**: the native messaging host only answers extension ids it knows. `src/ipc.rs` adds the store id to `CHROME_IDS` when the build has `STASH_CHROME_STORE_ID` set; the release job sets it from the `CHROME_EXTENSION_ID` secret at job level, so every release build accepts the store extension.

   Local builds can set it the same way (`$env:STASH_CHROME_STORE_ID = '<id>'` before `cargo build`). Without it, the app accepts only the unpacked extension, and the store extension falls back to browser downloads. The id never changes once the item exists, so it's equally fine to hard-code it in `CHROME_IDS`.

Each release then uploads `stash-chrome-store.zip` and submits it for review; it goes live when Google approves it (usually within a few days). A version must be higher than the one in the store, which the Cargo.toml version bump takes care of.

### Firefox (addons.mozilla.org)

1. **Account**: sign in to the [AMO Developer Hub](https://addons.mozilla.org/developers/) with a Firefox account.
2. **First submission, by hand**: select **Submit a New Add-on**, choose **On this site** (listed), and upload `browser\dist\stash-firefox.zip`. The gecko id `stash-downloads@frstycodes.github.io` in `manifest.firefox.json` claims the add-on's id. Answer "no" to the source code question (the code isn't minified or bundled), then fill in the listing from [store/listing.md](store/listing.md): summary, description, category (Download Management), support links, privacy policy and license (Stash uses the PolyForm Noncommercial license, so pick **Custom License** and paste `LICENSE.md`, or All Rights Reserved).
3. **API keys**: on [Manage API Keys](https://addons.mozilla.org/developers/addon/api/key/), generate credentials. The **JWT issuer** goes in the `AMO_JWT_ISSUER` secret and the **JWT secret** in `AMO_JWT_SECRET`.

Each release then runs `web-ext sign --channel=listed` on `browser\dist\firefox`, which submits the new version for review without waiting for it; it goes live when Mozilla approves it. Firefox needs nothing in `src/ipc.rs`: the host already allows `stash-downloads@frstycodes.github.io`, whatever the source.

### Edge

Brave installs straight from the Chrome Web Store, and Edge does once you allow extensions from other stores. A listing in the [Edge Add-ons store](https://partner.microsoft.com/dashboard/microsoftedge/) would get yet another id, which would need adding to `CHROME_IDS` the same way.
