"""Download benchmark: Stash's engine against curl, aria2c and a browser-style single
connection, on a local server with known behavior and on a few public files.

    cargo test --release --no-run
    python -I bench/bench.py [--repeat 3] [--only local] [--tools stash,curl,...]

Wall time, average speed, CPU time (user + kernel) and peak working set are measured
the same way for every tool, from this script, through the process handle. Stash's
peak speed comes from its own snapshot (1 s windows). Prints a Markdown table.
"""

import argparse
import ctypes
import ctypes.wintypes as wt
import glob
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
PORT = 47900
L = f"http://127.0.0.1:{PORT}/f"
MB = 1 << 20

# name -> (urls, note)
CASES = {
    # a fast line (100 MB/s link) where each connection is held to 6 MB/s (server
    # shaping, or the TCP window over a long round trip): what segmenting is for
    "local-big": ([f"{L}/big.bin?size={300 * MB}&rate={6 * MB}"], "300 MB, 6 MB/s per connection, 100 MB/s link"),
    "local-norange": ([f"{L}/nr.bin?size={100 * MB}&rate={6 * MB}&ranges=0"], "100 MB, no ranges, 6 MB/s per connection"),
    "local-slowstart": ([f"{L}/slow.bin?size={100 * MB}&rate={6 * MB}&ttfb=1500"], "100 MB, 1.5 s to first byte per request"),
    "local-maxconn4": ([f"{L}/lim.bin?size={100 * MB}&rate={6 * MB}&maxconn=4"], "100 MB, 429 past 4 connections"),
    "local-reqlimit": ([f"{L}/rl.bin?size={100 * MB}&rate={6 * MB}&maxreq=3"], "100 MB, 429 past 3 requests in 5 s"),
    "local-small": ([f"{L}/s{i}.bin?size={2 * MB}&rate={6 * MB}&ttfb=80" for i in range(30)], "30 x 2 MB, 80 ms to first byte"),
    # nothing holds it back: the engine's own CPU and write path
    "local-unlimited": ([f"{L}/fast.bin?size={1024 * MB}"], "1 GB, no per-connection cap (CPU/disk bound)"),
    "hetzner": (["https://fsn1-speed.hetzner.com/100MB.bin"], "100 MB, HTTP/1.1, ranges"),
    "github": (["https://github.com/rsms/inter/releases/download/v4.1/Inter-4.1.zip"], "33 MB GitHub release (redirect, Azure blob)"),
    "python": (["https://www.python.org/ftp/python/3.12.7/python-3.12.7-amd64.exe"], "26 MB from Fastly"),
    "jsdelivr": ([
        "https://cdn.jsdelivr.net/npm/three@0.160.0/build/three.module.js",
        "https://cdn.jsdelivr.net/npm/three@0.160.0/build/three.cjs",
        "https://cdn.jsdelivr.net/npm/typescript@5.3.3/lib/typescript.js",
        "https://cdn.jsdelivr.net/npm/typescript@5.3.3/lib/tsserver.js",
        "https://cdn.jsdelivr.net/npm/monaco-editor@0.45.0/min/vs/editor/editor.main.js",
        "https://cdn.jsdelivr.net/npm/pdfjs-dist@4.0.379/build/pdf.worker.mjs",
        "https://cdn.jsdelivr.net/npm/echarts@5.4.3/dist/echarts.js",
        "https://cdn.jsdelivr.net/npm/plotly.js-dist@2.27.1/plotly.js",
        "https://cdn.jsdelivr.net/npm/@babel/standalone@7.23.6/babel.js",
        "https://cdn.jsdelivr.net/npm/mathjax@3.2.2/es5/tex-mml-chtml.js",
        "https://cdn.jsdelivr.net/npm/lodash@4.17.21/lodash.js",
        "https://cdn.jsdelivr.net/npm/react-dom@18.2.0/umd/react-dom.development.js",
    ], "12 files, 0.5-10 MB, HTTP/2 CDN"),
}

TOOLS = ["stash", "browser", "curl", "aria2c"]


class FILETIME(ctypes.Structure):
    _fields_ = [("lo", wt.DWORD), ("hi", wt.DWORD)]


class PMC(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD), ("PeakWorkingSetSize", ctypes.c_size_t), ("WorkingSetSize", ctypes.c_size_t),
                ("a", ctypes.c_size_t), ("b", ctypes.c_size_t), ("c", ctypes.c_size_t), ("d", ctypes.c_size_t), ("PagefileUsage", ctypes.c_size_t),
                ("PeakPagefileUsage", ctypes.c_size_t)]


def proc_stats(handle):
    k = ctypes.windll.kernel32
    c, e, kt, ut = FILETIME(), FILETIME(), FILETIME(), FILETIME()
    k.GetProcessTimes(wt.HANDLE(handle), ctypes.byref(c), ctypes.byref(e), ctypes.byref(kt), ctypes.byref(ut))
    cpu = ((kt.hi << 32 | kt.lo) + (ut.hi << 32 | ut.lo)) / 1e7
    pmc = PMC()
    pmc.cb = ctypes.sizeof(PMC)
    ctypes.windll.psapi.GetProcessMemoryInfo(wt.HANDLE(handle), ctypes.byref(pmc), pmc.cb)
    return cpu, pmc.PeakWorkingSetSize


def test_exe():
    exes = [p for p in glob.glob(os.path.join(ROOT, "target", "release", "deps", "stash-*.exe"))]
    return max(exes, key=os.path.getmtime)


def aria2c():
    found = shutil.which("aria2c")
    if found:
        return found
    g = glob.glob(os.path.expandvars(r"%LOCALAPPDATA%\Microsoft\WinGet\Packages\aria2.aria2*\*\aria2c.exe"))
    return g[0] if g else None


def run_one(tool, urls, out, exe, conns, extra_env):
    env = dict(os.environ)
    if tool in ("stash", "browser"):
        env["STASH_BENCH_URLS"] = " ".join(urls)
        env["STASH_BENCH_CONNS"] = "1" if tool == "browser" else str(conns)
        # a browser runs every file at once (6 per host); Stash runs 3 at a time
        env["STASH_BENCH_MAX_ACTIVE"] = "6" if tool == "browser" else "3"
        env.update(extra_env)
        cmd = [exe, "dl::bench::run", "--ignored", "--exact", "--nocapture", "--test-threads=1"]
    elif tool == "curl":
        cmd = ["curl", "-sS", "-L", "--fail"]
        if len(urls) > 1:
            cmd += ["-Z", "--parallel-max", "6"]
        for i, u in enumerate(urls):
            cmd += ["-o", os.path.join(out, f"f{i}"), u]
    elif tool == "aria2c":
        cmd = [aria2c(), "-x16", "-s16", "-k1M", "-j5", "-d", out, "--console-log-level=error", "--summary-interval=0", "--allow-overwrite=true", "-q"]
        if len(urls) > 1:
            lst = os.path.join(out, "list.txt")
            with open(lst, "w") as f:
                f.write("\n".join(f"{u}\n  out=f{i}" for i, u in enumerate(urls)))
            cmd += ["-i", lst]
        else:
            cmd += ["-o", "f0", urls[0]]
    t0 = time.perf_counter()
    p = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
    stdout, _ = p.communicate()
    wall = time.perf_counter() - t0
    cpu, peak_mem = proc_stats(int(p._handle))
    res = {"wall": wall, "cpu": cpu, "mem": peak_mem, "ok": p.returncode == 0, "peak": None, "conns": None}
    if tool in ("stash", "browser"):
        line = next((l for l in stdout.splitlines() if "BENCH {" in l), None)
        if line:
            b = json.loads(line[line.index("BENCH {") + 6:])
            # the engine's own clock and CPU, without the byte-by-byte check that follows
            res.update(peak=b["peak"], bytes=b["bytes"], conns=b["max_conns"], wall=b["wall"], cpu=b["cpu"])
        if not res["ok"]:
            res["err"] = stdout[-600:]
    else:
        res["bytes"] = sum(os.path.getsize(os.path.join(out, f)) for f in os.listdir(out) if f.startswith("f"))
        if not res["ok"]:
            res["err"] = stdout[-600:]
    return res


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--repeat", type=int, default=3)
    ap.add_argument("--only", default="", help="comma-separated case names or prefixes")
    ap.add_argument("--tools", default=",".join(TOOLS))
    ap.add_argument("--conns", type=int, default=8)
    ap.add_argument("--link", type=int, default=100 * MB, help="local server's shared link cap, bytes/s")
    ap.add_argument("--label", default="")
    ap.add_argument("--json", default="")
    ap.add_argument("--env", action="append", default=[], help="K=V for Stash's runs (experiments)")
    a = ap.parse_args()
    exe = test_exe()
    tools = a.tools.split(",")
    want = [w for w in a.only.split(",") if w]
    cases = [c for c in CASES if not want or any(c == w or c.startswith(w) for w in want)]

    server = None
    if any(c.startswith("local") for c in cases):
        env = dict(os.environ, STASH_BENCH_PORT=str(PORT), STASH_BENCH_LINK=str(a.link))
        server = subprocess.Popen([exe, "dl::bench::serve", "--ignored", "--exact", "--nocapture"], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        time.sleep(1.5)
    rows = []
    try:
        for case in cases:
            urls, note = CASES[case]
            for tool in tools:
                if tool == "aria2c" and not aria2c():
                    continue
                runs = []
                for r in range(a.repeat):
                    out = tempfile.mkdtemp(prefix="stash-bench-out-")
                    try:
                        if case.startswith("local"):
                            # let the last run's connections close (they count against
                            # maxconn) and the server's 5 s request window empty
                            time.sleep(5.5 if any("maxreq" in u for u in urls) else 2.0)
                        res = run_one(tool, urls, out, exe, a.conns, dict(kv.split("=", 1) for kv in a.env))
                    finally:
                        shutil.rmtree(out, ignore_errors=True)
                    runs.append(res)
                    print(f"  {case:16} {tool:8} run {r + 1}: {res['wall']:.2f}s cpu {res['cpu']:.2f}s mem {res['mem'] / MB:.0f} MB"
                          + (f" peak {res['peak'] / MB:.1f} MB/s conns {res['conns']}" if res.get("peak") else "")
                          + ("" if res["ok"] else f"  FAILED {res.get('err', '')!r}"), file=sys.stderr, flush=True)
                ok = [x for x in runs if x["ok"]]
                if not ok:
                    rows.append({"case": case, "tool": tool, "failed": True, "note": note})
                    continue
                med = lambda k: statistics.median(x[k] for x in ok)
                wall = med("wall")
                rows.append({
                    "case": case, "tool": tool, "note": note, "runs": len(ok), "of": len(runs),
                    "wall": wall, "avg": statistics.median(x["bytes"] / x["wall"] for x in ok),
                    "peak": statistics.median(x["peak"] for x in ok) if all(x.get("peak") for x in ok) else None,
                    "cpu": med("cpu"), "mem": med("mem"),
                })
    finally:
        if server:
            server.kill()
    if a.json:
        with open(a.json, "w") as f:
            json.dump({"label": a.label, "rows": rows}, f, indent=1)
    print(f"\n{a.label}\n")
    print("| case | tool | wall s | avg MB/s | peak MB/s | CPU s | peak RAM MB | ok |")
    print("|---|---|--:|--:|--:|--:|--:|--:|")
    for r in rows:
        if r.get("failed"):
            print(f"| {r['case']} | {r['tool']} | failed | | | | | 0 |")
            continue
        pk = f"{r['peak'] / MB:.1f}" if r["peak"] else "-"
        print(f"| {r['case']} | {r['tool']} | {r['wall']:.2f} | {r['avg'] / MB:.1f} | {pk} | {r['cpu']:.2f} | {r['mem'] / MB:.0f} | {r['runs']}/{r['of']} |")


if __name__ == "__main__":
    main()
