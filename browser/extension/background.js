// Stash for Chrome, Edge and Firefox: hands the browser's downloads to the Stash app
// through native messaging (stash.exe is the host), then cancels the browser's copy.
// If Stash isn't installed or says no, the browser just downloads as usual.

const api = globalThis.browser ?? globalThis.chrome;
const HOST = "app.stash.downloads";

const defaults = { enabled: true, minSizeMB: 0 };

async function settings() {
  return { ...defaults, ...(await api.storage.local.get(Object.keys(defaults))) };
}

async function send(message) {
  try {
    return await api.runtime.sendNativeMessage(HOST, message);
  } catch (e) {
    return { ok: false, error: String(e?.message ?? e) };
  }
}

/** The cookies the site set, so downloads behind a sign-in still work in Stash. */
async function cookiesFor(url) {
  try {
    const list = await api.cookies.getAll({ url });
    return list.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

async function handOver(url, filename, referrer) {
  return send({
    type: "download",
    download: {
      url,
      filename: filename || null,
      referrer: referrer || null,
      cookies: await cookiesFor(url),
      userAgent: navigator.userAgent,
    },
  });
}

// Ids of downloads we gave back to the browser, so we don't grab them again.
const passThrough = new Set();

api.downloads.onCreated.addListener(async (item) => {
  const url = item.finalUrl || item.url;
  if (!/^https?:/i.test(url) || passThrough.has(item.id)) return;
  const s = await settings();
  if (!s.enabled) return;
  if (s.minSizeMB > 0 && item.totalBytes > 0 && item.totalBytes < s.minSizeMB * 1e6) return;
  const reply = await handOver(url, item.filename, item.referrer);
  if (!reply?.ok) return; // Stash isn't there: let the browser carry on
  try {
    await api.downloads.cancel(item.id);
    await api.downloads.erase({ id: item.id });
  } catch {
    // already finished (tiny files): nothing to cancel
  }
  flash();
});

// Right-click a link: "Download with Stash"
function menus() {
  api.contextMenus.removeAll(() => {
    api.contextMenus.create({ id: "stash-link", title: "Download with Stash", contexts: ["link", "image", "video", "audio"] });
  });
}
api.runtime.onInstalled.addListener(menus);
api.runtime.onStartup?.addListener(menus);

api.contextMenus.onClicked.addListener(async (info, tab) => {
  const url = info.linkUrl || info.srcUrl;
  if (!url) return;
  const reply = await handOver(url, null, info.pageUrl || tab?.url);
  if (reply?.ok) flash();
  else {
    // no Stash: fall back to the browser's own download
    const id = await api.downloads.download({ url });
    passThrough.add(id);
  }
});

// A little sunset badge blinks on the toolbar icon when something lands in Stash.
function flash() {
  api.action.setBadgeBackgroundColor({ color: "#f27d2e" });
  api.action.setBadgeText({ text: "↓" });
  setTimeout(() => api.action.setBadgeText({ text: "" }), 1600);
}

// The popup asks us things.
api.runtime.onMessage.addListener((msg, _sender, reply) => {
  if (msg?.type === "ping" || msg?.type === "show") {
    send({ type: msg.type }).then(reply);
    return true; // async reply
  }
  return false;
});
