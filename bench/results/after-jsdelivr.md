  jsdelivr         stash    run 1: 4.93s cpu 0.75s mem 17 MB peak 11.9 MB/s conns 19
  jsdelivr         stash    run 2: 4.83s cpu 1.27s mem 17 MB peak 11.7 MB/s conns 19
  jsdelivr         browser  run 1: 4.02s cpu 0.98s mem 17 MB peak 14.0 MB/s conns 6
  jsdelivr         browser  run 2: 4.02s cpu 1.06s mem 17 MB peak 14.0 MB/s conns 6

jsdelivr after fix

| case | tool | wall s | avg MB/s | peak MB/s | CPU s | peak RAM MB | ok |
|---|---|--:|--:|--:|--:|--:|--:|
| jsdelivr | stash | 4.88 | 8.8 | 11.8 | 1.01 | 17 | 2/2 |
| jsdelivr | browser | 4.02 | 10.7 | 14.0 | 1.02 | 17 | 2/2 |
