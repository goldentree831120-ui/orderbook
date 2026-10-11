# orderbook

A price-time priority limit order book in Rust. Pure library, no I/O, no runtime dependencies.

Status: **M1** core engine, **M2** property tests, **M3** benchmarks + O(1) cancel, **M4** deterministic replay, **M5** Solana/Anchor program in [`solana/`](solana/README.md). Next: M6 TypeScript client + final README.
Design rationale and per-operation complexity: [DESIGN.md](DESIGN.md).

```sh
cargo test                  # 17 scenario, 5 property, 10 replay tests + doctest
cargo run --example demo    # step-by-step walkthrough of a small book
./scripts/bench.sh          # criterion benchmarks, prints your hardware first
./scripts/mutation-check.sh # plants 6 bugs; the tests must catch each

# M4: replay a command log; same log -> same events, trades and final book, every time
cargo run --release --bin replay -- examples/sample.log --book
cargo run --release --bin replay -- --generate 100000 --seed 42 > big.log
cargo run --release --bin replay -- big.log --events big.events
cargo run --release --bin replay -- big.log --check big.events     # exit 1 on any difference
```

```rust
use orderbook::{OrderBook, OrderRequest, Side};

let mut book = OrderBook::new();
book.submit(OrderRequest::limit(1, Side::Sell, 101, 10));
let events = book.submit(OrderRequest::limit(2, Side::Buy, 101, 4));
// events: Accepted, Trade { maker 1, taker 2, price 101, qty 4 }
```

Layout:

```
src/types.rs     Order, OrderRequest, Trade, Event, ...
src/book.rs      OrderBook: submit, cancel, queries, invariants
tests/engine.rs      one test per matching rule + a 5,000-op randomised stress test
tests/properties.rs  proptest: invariants + comparison against a naive reference engine
tests/replay.rs      M4: golden log, two-process determinism, split replay, CLI --check
src/replay.rs        M4: log format, replay, canonical event log, digests, generator
src/bin/replay.rs    M4: command-line replay tool
benches/engine.rs    criterion: insert, cancel, match sweep, mixed flow
examples/demo.rs
```
