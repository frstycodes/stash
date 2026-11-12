  github           stash    run 1: 3.37s cpu 0.66s mem 17 MB peak 15.3 MB/s conns 8
  github           stash    run 2: 3.42s cpu 0.73s mem 17 MB peak 15.7 MB/s conns 8
  github           browser  run 1: 3.37s cpu 0.61s mem 17 MB peak 15.6 MB/s conns 1
  github           browser  run 2: 3.37s cpu 0.50s mem 17 MB peak 15.8 MB/s conns 1
  github           curl     run 1: 3.36s cpu 0.55s mem 9 MB
  github           curl     run 2: 3.38s cpu 0.69s mem 9 MB
  github           aria2c   run 1: 4.15s cpu 1.02s mem 25 MB
  github           aria2c   run 2: 3.91s cpu 1.00s mem 26 MB
  python           stash    run 1: 2.51s cpu 0.42s mem 16 MB peak 11.7 MB/s conns 8
  python           stash    run 2: 2.56s cpu 0.56s mem 16 MB peak 11.7 MB/s conns 8
  python           browser  run 1: 2.46s cpu 0.64s mem 16 MB peak 11.7 MB/s conns 1
  python           browser  run 2: 2.46s cpu 0.45s mem 16 MB peak 11.6 MB/s conns 1
  python           curl     run 1: 2.43s cpu 0.39s mem 9 MB
  python           curl     run 2: 2.43s cpu 0.33s mem 9 MB
  python           aria2c   run 1: 2.74s cpu 1.02s mem 30 MB
  python           aria2c   run 2: 2.76s cpu 0.70s mem 26 MB
  jsdelivr         stash    run 1: 0.85s cpu 0.22s mem 16 MB
  jsdelivr         stash    run 2: 0.80s cpu 0.27s mem 16 MB
  jsdelivr         browser  run 1: 0.75s cpu 0.25s mem 16 MB
  jsdelivr         browser  run 2: 0.76s cpu 0.27s mem 16 MB
  jsdelivr         curl     run 1: 0.03s cpu 0.02s mem 3 MB  FAILED ''
  jsdelivr         curl     run 2: 0.02s cpu 0.00s mem 3 MB  FAILED ''
  jsdelivr         aria2c   run 1: 0.04s cpu 0.00s mem 3 MB  FAILED ''
  jsdelivr         aria2c   run 2: 0.03s cpu 0.00s mem 3 MB  FAILED ''

after public

| case | tool | wall s | avg MB/s | peak MB/s | CPU s | peak RAM MB | ok |
|---|---|--:|--:|--:|--:|--:|--:|
| github | stash | 3.40 | 9.5 | 15.5 | 0.70 | 17 | 2/2 |
| github | browser | 3.37 | 9.5 | 15.7 | 0.55 | 17 | 2/2 |
| github | curl | 3.37 | 9.5 | - | 0.62 | 9 | 2/2 |
| github | aria2c | 4.03 | 8.0 | - | 1.01 | 26 | 2/2 |
| python | stash | 2.54 | 10.0 | 11.7 | 0.49 | 16 | 2/2 |
| python | browser | 2.46 | 10.3 | 11.6 | 0.55 | 16 | 2/2 |
| python | curl | 2.43 | 10.4 | - | 0.36 | 9 | 2/2 |
| python | aria2c | 2.75 | 9.2 | - | 0.86 | 28 | 2/2 |
| jsdelivr | stash | 0.83 | 8.4 | - | 0.24 | 16 | 2/2 |
| jsdelivr | browser | 0.75 | 9.2 | - | 0.26 | 16 | 2/2 |
| jsdelivr | curl | failed | | | | | 0 |
| jsdelivr | aria2c | failed | | | | | 0 |
