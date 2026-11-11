// Stash promo soundtrack: an original 128 BPM future-house cue, synthesized from scratch (no samples).
// 16 bars (30 s) + a 2 s tail. Writes music.wav next to this file.
//   node music.js
// The video (promo-video.html) is cut to the same grid: beat = 60/128 s, bar = 4 beats.
const fs = require("fs");
const path = require("path");

const SR = 44100, BPM = 128, BEAT = 60 / BPM, BAR = BEAT * 4, BARS = 16, TAIL = 2;
const N = Math.ceil((BARS * BAR + TAIL) * SR);
const bus = () => [new Float32Array(N), new Float32Array(N)];
const drums = bus(), music = bus(), send = bus(), fx = bus();

let seed = 7;
const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0) / 4294967296) * 2 - 1;
const mtof = (m) => 440 * Math.pow(2, (m - 69) / 12);
const at = (bar, beat = 0) => (bar * 4 + beat) * BEAT;

// one-pole and biquad filters
class Biquad {
  constructor(type) { this.type = type; this.x1 = this.x2 = this.y1 = this.y2 = 0; }
  set(f, q = 0.707) {
    f = Math.min(Math.max(f, 20), SR * 0.45);
    const w = 2 * Math.PI * f / SR, c = Math.cos(w), a = Math.sin(w) / (2 * q);
    let b0, b1, b2;
    if (this.type === "lp") { b0 = (1 - c) / 2; b1 = 1 - c; b2 = b0; }
    else if (this.type === "hp") { b0 = (1 + c) / 2; b1 = -(1 + c); b2 = b0; }
    else { b0 = a; b1 = 0; b2 = -a; } // band
    const a0 = 1 + a;
    this.b0 = b0 / a0; this.b1 = b1 / a0; this.b2 = b2 / a0; this.a1 = -2 * c / a0; this.a2 = (1 - a) / a0;
  }
  run(x) {
    const y = this.b0 * x + this.b1 * this.x1 + this.b2 * this.x2 - this.a1 * this.y1 - this.a2 * this.y2;
    this.x2 = this.x1; this.x1 = x; this.y2 = this.y1; this.y1 = y; return y;
  }
}

function put(b, i, l, r) { if (i >= 0 && i < N) { b[0][i] += l; b[1][i] += r; } }
const panLR = (p) => [Math.cos((p + 1) * Math.PI / 4), Math.sin((p + 1) * Math.PI / 4)];

// ---------- drums ----------
function kick(t, amp = 1) {
  const s = Math.floor(t * SR), len = 0.45 * SR; let ph = 0;
  for (let i = 0; i < len; i++) {
    const tt = i / SR, f = 46 + 120 * Math.exp(-tt * 30);
    ph += 2 * Math.PI * f / SR;
    const v = (Math.sin(ph) * Math.exp(-tt * 6.5) + rnd() * 0.25 * Math.exp(-tt * 400)) * amp * 0.95;
    put(drums, s + i, v, v);
  }
}
function clap(t, amp = 1) {
  const s = Math.floor(t * SR), len = 0.35 * SR, bp = new Biquad("band"); bp.set(1500, 0.9);
  for (let i = 0; i < len; i++) {
    const tt = i / SR;
    let e = Math.exp(-tt * 22);
    for (const o of [0, 0.011, 0.022]) if (tt >= o && tt < o + 0.01) e = Math.max(e, Math.exp(-(tt - o) * 200));
    const v = bp.run(rnd()) * e * amp * 1.6;
    put(drums, s + i, v, v); put(send, s + i, v * 0.35, v * 0.35);
  }
}
function hat(t, open = false, amp = 1) {
  const s = Math.floor(t * SR), len = (open ? 0.3 : 0.05) * SR, hp = new Biquad("hp"); hp.set(8000, 0.8);
  const [l, r] = panLR(open ? -0.2 : 0.25);
  for (let i = 0; i < len; i++) {
    const v = hp.run(rnd()) * Math.exp(-(i / SR) * (open ? 11 : 75)) * 0.32 * amp;
    put(drums, s + i, v * l, v * r);
  }
}

// ---------- tonal ----------
function saw(ph) { return 2 * (ph - Math.floor(ph + 0.5)); }
function voice(t, dur, midi, opts) {
  const { bus: b = music, amp = 0.2, detune = [0], pan = 0, cut = [800, 3000], decay = 0, a = 0.005, r = 0.08, q = 0.8, sendAmt = 0, sub = 0, sq = 0 } = opts;
  const s = Math.floor(t * SR), len = Math.floor((dur + r) * SR), f = mtof(midi), lp = new Biquad("lp");
  const phs = detune.map(() => Math.random()), [l, rr] = panLR(pan);
  let subph = 0;
  for (let i = 0; i < len; i++) {
    const tt = i / SR;
    if (i % 32 === 0) lp.set(cut[0] + (cut[1] - cut[0]) * Math.exp(-tt * (decay || 0.0001)), q);
    let v = 0;
    detune.forEach((d, k) => { phs[k] = (phs[k] + f * (1 + d) / SR) % 1; v += saw(phs[k]) + (sq ? sq * (phs[k] < 0.5 ? 1 : -1) : 0); });
    v /= Math.sqrt(detune.length);
    if (sub) { subph += f / 2 / SR; v += Math.sin(2 * Math.PI * subph) * sub; }
    let env = Math.min(1, tt / a);
    if (tt > dur) env *= Math.max(0, 1 - (tt - dur) / r);
    v = lp.run(v) * env * amp;
    put(b, s + i, v * l, v * rr); if (sendAmt) put(send, s + i, v * l * sendAmt, v * rr * sendAmt);
  }
}
const SUPER = [-0.012, -0.007, -0.003, 0, 0.003, 0.007, 0.012];

// ---------- fx ----------
function riser(t0, t1, amp = 0.35) {
  const s = Math.floor(t0 * SR), len = Math.floor((t1 - t0) * SR), bp = new Biquad("band"); let ph = 0;
  for (let i = 0; i < len; i++) {
    const x = i / len;
    if (i % 32 === 0) bp.set(300 * Math.pow(30, x), 1.2);
    ph += 2 * Math.PI * (200 + 1400 * x * x) / SR;
    const v = (bp.run(rnd()) * 1.4 + Math.sin(ph) * 0.15) * x * x * amp;
    put(fx, s + i, v * (1 - x * 0.5), v * (0.5 + x * 0.5)); put(send, s + i, v * 0.3, v * 0.3);
  }
}
function impact(t, amp = 1) {
  const s = Math.floor(t * SR), len = 2.4 * SR, lp = new Biquad("lp"); lp.set(2500); let ph = 0;
  for (let i = 0; i < len; i++) {
    const tt = i / SR; ph += 2 * Math.PI * (32 + 70 * Math.exp(-tt * 8)) / SR;
    const v = (Math.sin(ph) * Math.exp(-tt * 2.2) * 0.9 + lp.run(rnd()) * Math.exp(-tt * 5) * 0.35) * amp;
    put(fx, s + i, v, v); put(send, s + i, v * 0.4, v * 0.4);
  }
}
function whoosh(t, dur = 0.45, amp = 0.3, dir = 1) {
  const s = Math.floor(t * SR), len = Math.floor(dur * SR), bp = new Biquad("band");
  for (let i = 0; i < len; i++) {
    const x = i / len; if (i % 32 === 0) bp.set(dir > 0 ? 400 * Math.pow(15, x) : 6000 / Math.pow(15, x), 1.5);
    const e = Math.sin(Math.PI * x) ** 2, v = bp.run(rnd()) * e * amp * 1.5, p = (x * 2 - 1) * dir;
    const [l, r] = panLR(p * 0.8); put(fx, s + i, v * l, v * r);
  }
}
function tick(t, amp = 0.35) {
  const s = Math.floor(t * SR), hp = new Biquad("hp"); hp.set(2500);
  for (let i = 0; i < 0.03 * SR; i++) { const v = hp.run(rnd()) * Math.exp(-(i / SR) * 260) * amp; put(fx, s + i, v, v); }
}

// ---------- the arrangement ----------
// Am  F  C  G  (vi IV I V in C)
const CH = [
  { root: 45, pad: [57, 60, 64, 69], tones: [69, 72, 76, 81] },
  { root: 41, pad: [57, 60, 65, 69], tones: [69, 72, 77, 81] },
  { root: 48, pad: [55, 60, 64, 67], tones: [67, 72, 76, 79] },
  { root: 43, pad: [55, 59, 62, 67], tones: [67, 71, 74, 79] },
];
const chord = (bar) => CH[bar % 4];
const ARP = [0, 1, 2, 1, 3, 2, 1, 2, 0, 1, 2, 3, 2, 1, 0, 1];
// the hook (8ths, null = rest), one bar per chord
const HOOK = [
  [76, null, 74, 76, null, 79, 76, 74],
  [72, null, 72, 74, null, 76, null, 72],
  [72, null, 71, 72, null, 74, 76, 79],
  [74, null, 74, 71, null, 74, null, 67],
];

for (let bar = 0; bar < BARS; bar++) {
  const c = chord(bar);
  const drop = bar >= 4 && bar < 14, build = bar >= 2 && bar < 4, intro = bar < 2, outro = bar >= 14;

  // pads: filtered in the intro, open in the drop
  if (!outro || bar === 14) {
    const cut = intro ? [500, 900] : build ? [900 + (bar - 2) * 900, 2600] : [1800, 4200];
    for (const m of c.pad) voice(at(bar), BAR * (bar === 14 ? 2 : 1) - 0.02, m, { bus: music, amp: intro ? 0.075 : 0.065, detune: SUPER, pan: (m % 3 - 1) * 0.5, cut, decay: 1.5, a: intro ? 0.4 : 0.02, r: 0.3, sendAmt: 0.6 });
  }
  // intro: four stabs on the logo hits in bar 1
  if (bar === 1) for (let b = 0; b < 4; b++) for (const m of c.pad.map((x) => x + 12)) voice(at(1, b), 0.12, m, { amp: 0.07, detune: SUPER, cut: [6000, 1200], decay: 14, r: 0.2, sendAmt: 0.8, pan: (m % 2 ? 0.4 : -0.4) });

  // arp pluck, 16ths, with a dotted-8th echo through the send
  if (!outro) for (let k = 0; k < 16; k++) {
    if (intro && k % 2) continue;
    const m = c.tones[ARP[k]] + (drop ? 0 : -12);
    voice(at(bar, k / 4), BEAT / 4 * 0.9, m, { amp: drop ? 0.06 : 0.05, detune: [-0.004, 0.004], sq: 0.3, cut: [5500, 500], decay: 22, r: 0.05, sendAmt: 0.5, pan: k % 2 ? 0.35 : -0.35 });
  }

  // bass: offbeat 8ths, plus the sub
  if (!intro && !outro) for (let k = 0; k < 8; k++) {
    if (build && k % 2 === 0) continue;
    if (drop && k % 2 === 0 && k !== 0) continue;
    voice(at(bar, k / 2 + (k % 2 ? 0 : 0)), BEAT / 2 * 0.8, c.root - 12 + (k === 7 && drop ? 12 : 0) + 12, { amp: 0.17, detune: [-0.004, 0.004], cut: [2200, 260], decay: 16, r: 0.03, sub: 0.9, q: 1.4 });
  }

  // hook lead over the second half of the drop
  if (bar >= 8 && bar < 14) HOOK[bar % 4].forEach((m, k) => {
    if (m == null) return;
    voice(at(bar, k / 2), BEAT / 2 * 0.85, m + 12, { amp: 0.06, detune: [-0.006, 0, 0.006], sq: 0.2, cut: [7000, 1500], decay: 8, r: 0.12, sendAmt: 0.55, pan: 0.1 });
  });

  // drums
  for (let b = 0; b < 4; b++) {
    if (drop || (build && (bar === 3 || b % 2 === 0))) kick(at(bar, b));
    if (drop && (b === 1 || b === 3)) clap(at(bar, b));
    if (drop || build) hat(at(bar, b + 0.5), drop && b % 2 === 1);
    if (drop) { hat(at(bar, b + 0.25), false, 0.45); hat(at(bar, b + 0.75), false, 0.55); }
  }
  // snare roll into the drop
  if (bar === 3) for (let k = 0; k < 16; k++) if (k >= 8 || k % 2 === 0) clap(at(3, k / 4), 0.25 + k / 24);
}

// structure fx
riser(at(2), at(4), 0.4);
impact(at(4), 1.1);
riser(at(7, 2), at(8), 0.25);
impact(at(8), 0.7);
riser(at(13), at(14), 0.3);
impact(at(14), 1);
for (const t of [at(6), at(10), at(12)]) whoosh(t - 0.2, 0.45, 0.3, 1);
tick(at(5, 1.5), 0.5); // the mouse click
// outro: a final hit and its tail
kick(at(15)); clap(at(15)); impact(at(15), 0.8);
for (const m of [57, 60, 64, 69, 76]) voice(at(15), BEAT * 3, m, { amp: 0.07, detune: SUPER, cut: [5000, 900], decay: 2, r: 1.5, sendAmt: 0.9 });

// ---------- mixing ----------
// sidechain: music ducks under every kick in the build and drop
function duck(t) {
  if (t < at(2) || t >= at(15) + BEAT) return 1;
  const tb = (t % BEAT) / BEAT;
  return 1 - 0.65 * Math.exp(-tb * 7);
}
// a small Schroeder reverb on the send
function reverb(src) {
  const out = bus();
  const combs = [1557, 1617, 1491, 1422, 1277, 1356], aps = [225, 556];
  for (let ch = 0; ch < 2; ch++) {
    const x = src[ch], y = out[ch], spread = ch * 23;
    for (const d0 of combs) {
      const d = d0 + spread, buf = new Float32Array(d); let idx = 0, lp = 0;
      for (let i = 0; i < N; i++) {
        const o = buf[idx]; lp = o * 0.7 + lp * 0.3; buf[idx] = x[i] + lp * 0.8; idx = (idx + 1) % d; y[i] += o / combs.length;
      }
    }
    for (const d of aps) {
      const buf = new Float32Array(d); let idx = 0;
      for (let i = 0; i < N; i++) { const b = buf[idx], v = -y[i] + b; buf[idx] = y[i] + b * 0.5; idx = (idx + 1) % d; y[i] = v; }
    }
  }
  return out;
}
const verb = reverb(send);
const L = new Float32Array(N), R = new Float32Array(N);
for (let i = 0; i < N; i++) {
  const g = duck(i / SR);
  L[i] = drums[0][i] * 0.8 + music[0][i] * g * 1.7 + verb[0][i] * 0.7 * (0.4 + 0.6 * g) + fx[0][i];
  R[i] = drums[1][i] * 0.8 + music[1][i] * g * 1.7 + verb[1][i] * 0.7 * (0.4 + 0.6 * g) + fx[1][i];
}
// glue: soft clip, normalise to -1 dBFS, 20 ms fade in, fade the tail
let peak = 0;
for (let i = 0; i < N; i++) { L[i] = Math.tanh(L[i] * 1.3); R[i] = Math.tanh(R[i] * 1.3); peak = Math.max(peak, Math.abs(L[i]), Math.abs(R[i])); }
const gain = Math.pow(10, -1 / 20) / peak, fadeIn = 0.02 * SR, fadeOutAt = N - 1.2 * SR;
const pcm = Buffer.alloc(N * 4);
for (let i = 0; i < N; i++) {
  let g = gain * Math.min(1, i / fadeIn); if (i > fadeOutAt) g *= Math.max(0, 1 - (i - fadeOutAt) / (N - fadeOutAt));
  pcm.writeInt16LE(Math.round(Math.max(-1, Math.min(1, L[i] * g)) * 32767), i * 4);
  pcm.writeInt16LE(Math.round(Math.max(-1, Math.min(1, R[i] * g)) * 32767), i * 4 + 2);
}
const head = Buffer.alloc(44);
head.write("RIFF", 0); head.writeUInt32LE(36 + pcm.length, 4); head.write("WAVE", 8); head.write("fmt ", 12);
head.writeUInt32LE(16, 16); head.writeUInt16LE(1, 20); head.writeUInt16LE(2, 22); head.writeUInt32LE(SR, 24);
head.writeUInt32LE(SR * 4, 28); head.writeUInt16LE(4, 32); head.writeUInt16LE(16, 34); head.write("data", 36); head.writeUInt32LE(pcm.length, 40);
fs.writeFileSync(path.join(__dirname, "music.wav"), Buffer.concat([head, pcm]));
console.log(`music.wav: ${(N / SR).toFixed(2)} s`);
