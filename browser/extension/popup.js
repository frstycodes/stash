const api = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);

api.storage.local.get({ enabled: true }).then(({ enabled }) => {
  $("enabled").checked = enabled;
});
$("enabled").addEventListener("change", (e) => api.storage.local.set({ enabled: e.target.checked }));

api.runtime.sendMessage({ type: "ping" }).then((r) => {
  const ok = !!r?.ok;
  $("dot").className = "dot " + (ok ? "on" : "off");
  $("status").textContent = ok ? `Connected · Stash ${r.version}` : "Not connected";
  $("hint").hidden = ok;
  $("open").disabled = !ok;
  $("open").style.opacity = ok ? 1 : 0.5;
});

$("open").addEventListener("click", async () => {
  await api.runtime.sendMessage({ type: "show" });
  window.close();
});
