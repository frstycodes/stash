// Renders promo-video.html frame by frame with Electron and pipes the frames to ffmpeg, muxed with music.wav.
//   <repo root>\node_modules\.bin\electron render.js                 -> stash-promo.mp4 (1080p60)
//   <repo root>\node_modules\.bin\electron render.js --stills 1,7.6,12  -> stills\t1.png ...
// Needs ffmpeg on PATH. Run `node music.js` first.
const { app, BrowserWindow } = require("electron");
const { spawn } = require("child_process");
const path = require("path");
const fs = require("fs");

const W = 1920, H = 1080, FPS = 60, DUR = 32;
app.commandLine.appendSwitch("force-device-scale-factor", "1");
app.commandLine.appendSwitch("disable-renderer-backgrounding");
const args = process.argv.slice(2);
const stillsArg = args.includes("--stills") ? args[args.indexOf("--stills") + 1] : null;

app.whenReady().then(async () => {
  const win = new BrowserWindow({
    width: W, height: H, useContentSize: true, show: false, frame: false, paintWhenInitiallyHidden: true,
    webPreferences: { backgroundThrottling: false, offscreen: true },
  });
  win.webContents.setFrameRate(60);
  await win.loadFile(path.join(__dirname, "promo-video.html"));
  await win.webContents.executeJavaScript("window.ready");
  const draw = async (t) => {
    await win.webContents.executeJavaScript(`render(${t}); new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))`);
    win.webContents.invalidate();
    return win.webContents.capturePage({ x: 0, y: 0, width: W, height: H });
  };

  if (stillsArg) {
    const dir = path.join(__dirname, "stills");
    fs.mkdirSync(dir, { recursive: true });
    for (const s of stillsArg.split(",")) {
      const img = await draw(parseFloat(s));
      fs.writeFileSync(path.join(dir, `t${s}.png`), img.toPNG());
      console.log(`stills/t${s}.png ${img.getSize().width}x${img.getSize().height}`);
    }
    app.quit(); return;
  }

  const out = path.join(__dirname, "stash-promo.mp4");
  const ff = spawn("ffmpeg", [
    "-v", "error", "-y",
    "-f", "rawvideo", "-pix_fmt", "bgra", "-s", `${W}x${H}`, "-r", String(FPS), "-i", "-",
    "-i", path.join(__dirname, "music.wav"),
    "-c:v", "libx264", "-preset", "slow", "-crf", "16", "-pix_fmt", "yuv420p", "-movflags", "+faststart",
    "-c:a", "aac", "-b:a", "256k", "-shortest", out,
  ], { stdio: ["pipe", "inherit", "inherit"] });
  const total = FPS * DUR;
  const t0 = Date.now();
  for (let f = 0; f < total; f++) {
    const img = await draw(f / FPS);
    const { width, height } = img.getSize();
    let buf = img.toBitmap();
    if (width !== W || height !== H) buf = img.resize({ width: W, height: H }).toBitmap();
    if (!ff.stdin.write(buf)) await new Promise((r) => ff.stdin.once("drain", r));
    if (f % 120 === 0) console.log(`frame ${f}/${total} (${((Date.now() - t0) / 1000).toFixed(0)} s)`);
  }
  ff.stdin.end();
  ff.on("close", (code) => { console.log(code === 0 ? `wrote ${out}` : `ffmpeg exited ${code}`); app.quit(); });
});
