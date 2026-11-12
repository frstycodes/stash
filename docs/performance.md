# Download performance

How fast Stash downloads compared with a browser, curl and aria2c (a stand-in for
FDM/IDM, which split files the same way), before and after the November 2026 engine
work, and what was tried along the way.

## How it's measured

`bench/bench.py` runs every tool the same way and measures from outside the process
(wall time, CPU time = user + kernel, peak working set). Stash runs as the real engine
(`dl::bench::run`, an ignored test) and also reports its peak speed over 1 s windows.

```
cargo test --release --no-run
python -I bench/bench.py --repeat 3                 # everything
python -I bench/bench.py --only local --tools stash # just Stash on the local cases
```

- **Stash**: default settings (8 connections, 3 downloads at a time).
- **Browser-style**: the same engine held to 1 connection per file, 6 files at a time.
  That's how Chrome and Edge download.
- **curl**: one connection per file, `-Z --parallel-max 6` for many files.
- **aria2c** `-x16 -s16 -k1M -j5`: up to 16 connections per file, as FDM/IDM do.

Two kinds of case:

- **Local** (`dl::bench::serve`, a small HTTP/1.1 server started by the script):
  repeatable, and set per URL. It has a 100 MB/s shared link, a cap per connection
  (`rate`), time to first byte (`ttfb`), no ranges, a limit on concurrent connections
  (`maxconn`) or on requests per 5 s (`maxreq`). It sends a known byte pattern, and
  every downloaded byte is checked against it.
- **Public**: real files over this machine's line (about 10 to 12 MB/s), 2 runs each
  (1 on Hetzner, which rate-limits). A single connection already fills this line on
  most CDNs, so the public cases mostly show that nothing got worse and that
  difficult servers now work. The local cases show what segmenting can win.

Numbers are medians. CPU times under 0.5 s are mostly noise: identical runs varied
from 0.1 to 2 s.

## Results

### Local server (3 runs each)

| case | 0.5.0 | **now** | browser-style | curl | aria2c |
|---|--:|--:|--:|--:|--:|
| 300 MB, 6 MB/s per connection | 6.85 s | **3.97 s** | 50.0 s | 50.0 s | 3.85 s |
| 100 MB, 1.5 s to first byte, 6 MB/s per connection | 7.60 s | **5.83 s** | 18.2 s | 18.2 s | 4.67 s |
| 100 MB, server turns away more than 4 connections (429) | **fails** | **4.47 s** | 16.7 s | 16.7 s | 5.63 s |
| 100 MB, server turns away more than 3 requests per 5 s (429) | **fails** | **7.79 s** | 16.7 s | 16.7 s | 14.7 s (2 of 3 runs failed) |
| 30 files × 2 MB, 80 ms to first byte | 11.58 s | **2.82 s** | 2.21 s | 2.64 s | 4.89 s |
| 100 MB, no range support | 16.71 s | 16.71 s | 16.70 s | 16.69 s | 16.74 s |
| 1 GB, nothing capped (CPU/disk bound) | 10.47 s | 11.15 s | 10.02 s | 10.05 s | 12.62 s |

Peak memory (working set) of the whole process:

| case | 0.5.0 | now | browser-style | curl | aria2c |
|---|--:|--:|--:|--:|--:|
| 300 MB, 6 MB/s per connection | 14 MB | 19 MB | 11 MB | 7 MB | 28 MB |
| 30 files × 2 MB | 13 MB | 15 MB | 15 MB | 7 MB | 20 MB |
| 1 GB, nothing capped | 17 MB | 20 MB | 12 MB | 7 MB | 28 MB |

### Public files (this machine's line, about 10 to 12 MB/s)

| file | 0.5.0 | **now** | browser-style | curl | aria2c |
|---|--:|--:|--:|--:|--:|
| GitHub release, 33 MB (redirect, HTTP/2) | 3.65 s | **3.40 s** | 3.37 s | 3.37 s | 4.03 s |
| python.org via Fastly, 26 MB (HTTP/2) | 2.62 s | **2.54 s** | 2.46 s | 2.43 s | 2.75 s |
| jsdelivr, 12 files, 41 MB (HTTP/2) | 4.45 s, **but saved 7.3 MB: truncated files** | **4.88 s, complete** | 4.02 s | 3.96 s | 4.28 s |
| Hetzner mirror, 100 MB (rate-limits requests) | **fails (429)** | **14.4 s** | 13.2 s | 16.6 s* | 24.3 s* |

\* from an earlier run the same day, before the final engine changes; both are tools
other than Stash, so unaffected.

### What it adds up to

- **Faster than a browser** wherever one connection can't fill the line: 4 to 12×
  faster on the local cases (with per-connection caps or a slow first byte).
- **On par with aria2c (FDM/IDM-style)** on big files: 3.97 s vs 3.85 s. Stash uses
  about two thirds of its memory (19 vs 28 MB).
- **Ahead of aria2c** on servers that limit connections or requests: 4.5 s vs 5.6 s,
  and 7.8 s vs 14.7 s (where aria2c also failed 2 runs of 3).
- **Ahead of aria2c** on many small files: 2.8 s vs 4.9 s.
- **No longer fails** on Hetzner and OVH mirrors, which answer 429 to more than 2 to 4
  connections. 0.5.0 treated that answer as fatal.
- **No longer saves truncated files** from servers whose range answers don't add up
  (jsdelivr; see below).
- **When one connection already fills the line** (the public CDNs here), Stash is
  within a few percent of the browser and curl.
- **Costs:** a few MB more memory while ramping up connections. On the uncapped 1 GB
  case it's ~6% slower than one connection (12 to 16 connections where 1 would do;
  on localhost they also compete with the server for CPU).

## What changed in the engine (`src/dl/http.rs`), and why

Each change below was measured on its own against the version before it.

1. **Extra connections open at once**, not 150 ms apart. The last of 8 connections
   slept ~1 s before finding nothing left to do, and the download wasn't done until it
   woke. Every download took at least a second: 30 small files went from 11.6 s to
   2.9 s. The 300 MB file went from 6.86 s to 6.29 s.
2. **"Too many requests" (429/503) no longer fails the download.** A connection that is
   turned away before it gets any data just leaves, and the others carry on. A
   connection that was already working retries after half a second (other connections
   finishing at the same moment can briefly push a server over its limit). The host's
   limit is remembered for 30 minutes, so the next download from it starts with that
   many connections instead of asking again. Every 10 s, while plenty is left, the
   engine tries one more connection, in case the limit was momentary.
   - Before: a server limiting to 4 connections failed the download outright.
   - After: 8.4 s with this alone, and 4.47 s once the next three changes were added.
3. **A turned-away connection's range is read by its neighbour.** Each range remembers
   how far the request reading it runs. When a range is handed back and the connection
   just before it is still reading a request that covers it, that connection reads on
   through. After a refusal, new requests ask for everything to the end of the file
   (`bytes=pos-`), so a connection can read on into any free range after its own.
   Request-limited server: 22.6 s became 9.9 s, then 7.8 s.
4. **Fewer splits after a refusal.** Once a server has turned someone away, ranges are
   only split when each half is at least 4 MiB: every split is another request it may
   refuse. (Turning splitting off entirely left one connection crawling through the
   tail: 6.3 s instead of 4.5 s.)
5. **Splits account for the time to first byte.** The engine measures how long requests
   take to answer and how fast each connection goes. A split only happens when there's
   more left than the current connection gets through while the new request waits. The
   split point is placed where both connections finish together, not at the midpoint.
   1.5 s first byte: 7.50 s became 5.84 s, no change elsewhere.
6. **Adaptive connection count.** After the first second, the engine adds 4 connections,
   then 4 more each second while the speed keeps rising by more than 15%. The limit is
   twice the setting (16 by default). There is no ramp under a speed limit, with a
   1-connection setting, or once the server has refused a connection. 300 MB with
   6 MB/s per connection: 6.29 s became 3.95 s, level with aria2c's 16 connections.
7. **Quick reconnect.** A connection cut off mid-transfer after bringing data reconnects
   at once (jsdelivr does this to some range requests). Other retries back off 0.5, 1,
   2, 4 s… instead of 2, 4, 8, 16 s. Their range goes back to the pool while they wait.
   A download is finished as soon as no bytes are left, without waiting on a connection
   that's asleep before a retry. A connection that runs out of retries now just leaves;
   the download fails only when the last one does.
8. **Ranges are only trusted when they add up.** jsdelivr answers `Range: bytes=0-`
   with `206`, a `Content-Range` total equal to the size of its *compressed* copy (e.g.
   74,460 bytes for lodash.js), no `Content-Length`, and then the whole 544,098-byte
   file. 0.5.0 believed the header and saved the first 74,460 bytes. Ranges are now used
   only when the answer's length matches its `Content-Range`. Otherwise the file comes
   in one piece, to the end of the stream. Covered by `dl::bench::lying_ranges`, which
   fails on 0.5.0 (400,000 of 3,145,728 bytes).

Nothing changed in `torrent.rs`, `limiter.rs` (one accessor added) or the defaults in
`Config`/`settings.rs`.

## Tried and dropped

- **Writing in 256 KiB batches** instead of one `seek_write` per received chunk: no
  measurable CPU difference, even at 100 MB/s, and +5 MB memory. The OS write cache
  already absorbs the small writes.
- **`MIN_SPLIT` of 256 KiB** (from 512 KiB): better on 2 MB files (2.92 s → 2.52 s),
  worse with a slow first byte (7.2 s → 8.3 s). The TTFB-aware split (change 5) is the
  better answer.
- **HTTP/1.1 only**: GitHub, python.org and jsdelivr speak HTTP/2 to Stash. All of a
  download's ranges then share one TCP connection, which defeats segmenting when a
  connection is the bottleneck. On this line one connection already fills the link,
  so forcing HTTP/1.1 made no measurable difference (3.50 vs 3.57 s, 2.72 vs 2.51 s).
  Left alone until it can be measured on a faster line.
- **A settling second before the ramp measures**: meant to stop the uncapped case from
  adding connections it doesn't need. It didn't stop that (noise), and it slowed the
  300 MB case to 4.6 to 4.9 s. Reverted.
- **Socket buffer sizes / TCP_NODELAY**: not changed. Setting `SO_RCVBUF` on Windows
  turns off receive-window autotuning, which is what makes one connection fast on a
  long, fat line. reqwest already sets `TCP_NODELAY`.
- **DNS caching / connection pooling**: already covered. The engine shares one
  `reqwest::Client` across all downloads (one pool per host). Windows caches DNS, and
  name lookups never showed up in the traces.
- **Torrent settings**: not changed. There's no repeatable torrent benchmark (the swarm
  varies far more than any setting), and the network test gets data from peers within
  5 s with librqbit's defaults.

## Recommendation, not changed

- **More downloads at once for small files.** With the line full, each new file spends
  0.4 to 0.6 s waiting for its first byte. With 3 at a time those gaps show: 12 jsdelivr
  files took 4.93 s at 3, 4.32 s at 6, and 4.02 s browser-style.
  - Raising the default `max_active` to 6 (in `settings.rs`) would close most of that.
  - Or the scheduler could let files under a few MB run beside the 3 big ones.
  - Either is a product decision: 6 big downloads × up to 16 connections is a lot to
    put on one line.

## Tests

- `cargo test`: unit tests for the new range logic (reading on, splitting, host limits).
- `cargo test --release dl::bench::pause_resume -- --ignored --nocapture`: pause at a
  third, resume, check every byte. Runs against a normal server, one limiting
  connections and one limiting requests.
- `dl::bench::limit`: the speed limit holds (4.04 MB/s against 4 MB/s).
- `dl::bench::lying_ranges`: the jsdelivr case above.
- The network tests `dl::tests::{http, early_icon, early_photo, torrent}` all pass.

Raw results: `bench/results/`.
