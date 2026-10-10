# orderbook

A price-time priority limit order book in Rust. Pure library, no I/O, no dependencies.

Status: **M1 — core engine** (limit and market orders, cancel, partial fills).
Design rationale and per-operation complexity: [DESIGN.md](DESIGN.md).

```sh
cargo test                  # scenario tests + doctest
cargo run --example demo    # step-by-step walkthrough of a small book
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
tests/engine.rs  one test per matching rule + a 5,000-op randomised stress test
examples/demo.rs
```
