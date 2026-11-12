  local-big        stash    run 1: 6.85s cpu 0.03s mem 14 MB peak 64.9 MB/s conns 8
  local-big        stash    run 2: 6.85s cpu 0.17s mem 14 MB peak 65.0 MB/s conns 8
  local-big        stash    run 3: 6.84s cpu 0.22s mem 14 MB peak 64.5 MB/s conns 8
  local-norange    stash    run 1: 16.71s cpu 0.08s mem 11 MB peak 7.4 MB/s conns 1
  local-norange    stash    run 2: 16.71s cpu 0.14s mem 12 MB peak 7.5 MB/s conns 1
  local-norange    stash    run 3: 16.71s cpu 0.09s mem 12 MB peak 7.3 MB/s conns 1
  local-slowstart  stash    run 1: 7.60s cpu 0.03s mem 14 MB peak 45.0 MB/s conns 8
  local-slowstart  stash    run 2: 7.60s cpu 0.03s mem 14 MB peak 45.0 MB/s conns 8
  local-slowstart  stash    run 3: 8.31s cpu 0.02s mem 14 MB peak 45.0 MB/s conns 8
  local-maxconn4   stash    run 1: 0.66s cpu 0.02s mem 12 MB  FAILED '"avg":0,"peak":0,"max_conns":4,"failed":["http://127.0.0.1:47900/f/lim.bin?size=104857600&rate=6291456&maxconn=4: Some(\\"The server answered 429 Too Many Requests\\")"]}\n\nthread \'dl::bench::run\' (20800) panicked at src\\dl\\bench.rs:238:5:\nfailed: ["http://127.0.0.1:47900/f/lim.bin?size=104857600&rate=6291456&maxconn=4: Some(\\"The server answered 429 Too Many Requests\\")"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\nFAILED\n\nfailures:\n\nfailures:\n    dl::bench::run\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.66s\n\n'
  local-maxconn4   stash    run 2: 0.65s cpu 0.02s mem 12 MB  FAILED '"avg":0,"peak":0,"max_conns":4,"failed":["http://127.0.0.1:47900/f/lim.bin?size=104857600&rate=6291456&maxconn=4: Some(\\"The server answered 429 Too Many Requests\\")"]}\n\nthread \'dl::bench::run\' (21532) panicked at src\\dl\\bench.rs:238:5:\nfailed: ["http://127.0.0.1:47900/f/lim.bin?size=104857600&rate=6291456&maxconn=4: Some(\\"The server answered 429 Too Many Requests\\")"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\nFAILED\n\nfailures:\n\nfailures:\n    dl::bench::run\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.66s\n\n'
  local-maxconn4   stash    run 3: 0.66s cpu 0.02s mem 12 MB  FAILED '"avg":0,"peak":0,"max_conns":4,"failed":["http://127.0.0.1:47900/f/lim.bin?size=104857600&rate=6291456&maxconn=4: Some(\\"The server answered 429 Too Many Requests\\")"]}\n\nthread \'dl::bench::run\' (11204) panicked at src\\dl\\bench.rs:238:5:\nfailed: ["http://127.0.0.1:47900/f/lim.bin?size=104857600&rate=6291456&maxconn=4: Some(\\"The server answered 429 Too Many Requests\\")"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\nFAILED\n\nfailures:\n\nfailures:\n    dl::bench::run\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.66s\n\n'
  local-reqlimit   stash    run 1: 0.50s cpu 0.02s mem 12 MB  FAILED 's":0,"avg":0,"peak":0,"max_conns":0,"failed":["http://127.0.0.1:47900/f/rl.bin?size=104857600&rate=6291456&maxreq=3: Some(\\"The server answered 429 Too Many Requests\\")"]}\n\nthread \'dl::bench::run\' (9956) panicked at src\\dl\\bench.rs:238:5:\nfailed: ["http://127.0.0.1:47900/f/rl.bin?size=104857600&rate=6291456&maxreq=3: Some(\\"The server answered 429 Too Many Requests\\")"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\nFAILED\n\nfailures:\n\nfailures:\n    dl::bench::run\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.51s\n\n'
  local-reqlimit   stash    run 2: 0.50s cpu 0.03s mem 12 MB  FAILED '":0,"avg":0,"peak":0,"max_conns":0,"failed":["http://127.0.0.1:47900/f/rl.bin?size=104857600&rate=6291456&maxreq=3: Some(\\"The server answered 429 Too Many Requests\\")"]}\n\nthread \'dl::bench::run\' (20516) panicked at src\\dl\\bench.rs:238:5:\nfailed: ["http://127.0.0.1:47900/f/rl.bin?size=104857600&rate=6291456&maxreq=3: Some(\\"The server answered 429 Too Many Requests\\")"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\nFAILED\n\nfailures:\n\nfailures:\n    dl::bench::run\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.51s\n\n'
  local-reqlimit   stash    run 3: 0.50s cpu 0.00s mem 12 MB  FAILED '":0,"avg":0,"peak":0,"max_conns":0,"failed":["http://127.0.0.1:47900/f/rl.bin?size=104857600&rate=6291456&maxreq=3: Some(\\"The server answered 429 Too Many Requests\\")"]}\n\nthread \'dl::bench::run\' (25000) panicked at src\\dl\\bench.rs:238:5:\nfailed: ["http://127.0.0.1:47900/f/rl.bin?size=104857600&rate=6291456&maxreq=3: Some(\\"The server answered 429 Too Many Requests\\")"]\nnote: run with `RUST_BACKTRACE=1` environment variable to display a backtrace\nFAILED\n\nfailures:\n\nfailures:\n    dl::bench::run\n\ntest result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.51s\n\n'
  local-small      stash    run 1: 11.58s cpu 0.03s mem 13 MB peak 6.6 MB/s conns 6
  local-small      stash    run 2: 11.57s cpu 0.14s mem 13 MB peak 6.6 MB/s conns 6
  local-small      stash    run 3: 11.59s cpu 0.05s mem 14 MB peak 6.6 MB/s conns 6
  local-unlimited  stash    run 1: 10.26s cpu 0.08s mem 16 MB peak 129.0 MB/s conns 8
  local-unlimited  stash    run 2: 10.47s cpu 0.05s mem 17 MB peak 125.2 MB/s conns 8
  local-unlimited  stash    run 3: 10.57s cpu 1.33s mem 17 MB peak 140.3 MB/s conns 8

0.5.0

| case | tool | wall s | avg MB/s | peak MB/s | CPU s | peak RAM MB | ok |
|---|---|--:|--:|--:|--:|--:|--:|
| local-big | stash | 6.85 | 43.8 | 64.9 | 0.17 | 14 | 3/3 |
| local-norange | stash | 16.71 | 6.0 | 7.4 | 0.09 | 12 | 3/3 |
| local-slowstart | stash | 7.60 | 13.2 | 45.0 | 0.03 | 14 | 3/3 |
| local-maxconn4 | stash | failed | | | | | 0 |
| local-reqlimit | stash | failed | | | | | 0 |
| local-small | stash | 11.58 | 5.2 | 6.6 | 0.05 | 13 | 3/3 |
| local-unlimited | stash | 10.47 | 97.8 | 129.0 | 0.08 | 17 | 3/3 |
