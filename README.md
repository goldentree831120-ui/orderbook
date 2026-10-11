# orderbook

A price-time priority limit order book in Rust. Pure library, no I/O, no runtime dependencies.

Status: **M1** core engine, **M2** property tests, **M3** benchmarks + O(1) cancel. Next: M4 replay, M5 Solana port.
Design rationale and per-operation complexity: [DESIGN.md](DESIGN.md).

```sh
cargo test                  # 17 scenario tests, 5 property tests (thousands of random cases), doctest
cargo run --example demo    # step-by-step walkthrough of a small book
./scripts/bench.sh          # criterion benchmarks, prints your hardware first
./scripts/mutation-check.sh # plants 6 bugs; the tests must catch each
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
benches/engine.rs    criterion: insert, cancel, match sweep, mixed flow
examples/demo.rs
```
