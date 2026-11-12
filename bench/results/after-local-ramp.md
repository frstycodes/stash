  local-big        stash    run 1: 3.97s cpu 0.05s mem 19 MB peak 100.6 MB/s conns 16
  local-big        stash    run 2: 3.97s cpu 0.06s mem 19 MB peak 100.6 MB/s conns 16
  local-big        stash    run 3: 4.02s cpu 0.03s mem 19 MB peak 100.6 MB/s conns 16
  local-slowstart  stash    run 1: 5.83s cpu 0.02s mem 16 MB peak 46.5 MB/s conns 16
  local-slowstart  stash    run 2: 5.83s cpu 0.39s mem 16 MB peak 46.3 MB/s conns 16
  local-slowstart  stash    run 3: 5.83s cpu 0.09s mem 15 MB peak 47.5 MB/s conns 16
  local-unlimited  stash    run 1: 10.61s cpu 0.19s mem 22 MB peak 122.1 MB/s conns 16
  local-unlimited  stash    run 2: 11.15s cpu 0.78s mem 20 MB peak 119.4 MB/s conns 16
  local-unlimited  stash    run 3: 11.26s cpu 1.61s mem 20 MB peak 127.9 MB/s conns 16

after (final ramp)

| case | tool | wall s | avg MB/s | peak MB/s | CPU s | peak RAM MB | ok |
|---|---|--:|--:|--:|--:|--:|--:|
| local-big | stash | 3.97 | 75.5 | 100.6 | 0.05 | 19 | 3/3 |
| local-slowstart | stash | 5.83 | 17.1 | 46.5 | 0.09 | 16 | 3/3 |
| local-unlimited | stash | 11.15 | 91.8 | 122.1 | 0.78 | 20 | 3/3 |
