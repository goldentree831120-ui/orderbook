//! M3 benchmarks. Run everything with `./scripts/bench.sh` (prints hardware info first).
//!
//! Each benchmark builds its book in an untimed setup (`iter_batched`), then times ONE
//! operation, so we measure the operation and not the setup. The routine RETURNS the book:
//! criterion drops returned values after timing. (The first version let the book drop inside
//! the routine, which timed the destructor of a 10,000-level book and made an O(log P) insert
//! look O(P). Lesson: check that benchmark scaling matches the complexity you expect.) Book shapes are chosen to
//! show how cost scales with the things the complexity analysis says matter:
//!   insert  -> number of price levels P
//!   cancel  -> orders at the same level k (the O(k) scan this milestone targets)
//!   match   -> number of levels swept L
use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput};
use orderbook::{OrderBook, OrderRequest, Side};
use std::hint::black_box;

/// `levels` ask levels starting at 1000, `per_level` orders of qty 10 each. Ids 1..
fn book_with_asks(levels: u64, per_level: u64) -> (OrderBook, u64) {
    let mut b = OrderBook::new();
    let mut id = 1;
    for l in 0..levels {
        for _ in 0..per_level {
            b.submit(OrderRequest::limit(id, Side::Sell, 1000 + l, 10));
            id += 1;
        }
    }
    (b, id)
}

fn insert(c: &mut Criterion) {
    let mut g = c.benchmark_group("insert_resting_limit");
    for levels in [10u64, 1_000, 10_000] {
        g.bench_with_input(
            BenchmarkId::from_parameter(levels),
            &levels,
            |bch, &levels| {
                bch.iter_batched(
                    || book_with_asks(levels, 1),
                    // A bid below every ask: no match, just rests (new level).
                    |(mut b, id)| {
                        let ev = b.submit(OrderRequest::limit(id, Side::Buy, 500, 10));
                        (b, ev) // return the book: criterion drops outputs OUTSIDE the timed region
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }
    g.finish();
}

fn cancel(c: &mut Criterion) {
    let mut g = c.benchmark_group("cancel_middle_of_level");
    for per_level in [10u64, 100, 1_000, 10_000] {
        g.bench_with_input(
            BenchmarkId::from_parameter(per_level),
            &per_level,
            |bch, &k| {
                bch.iter_batched(
                    || book_with_asks(1, k),
                    // Cancel the order in the middle of the single level's queue.
                    |(mut b, _)| {
                        let ev = b.cancel(k / 2 + 1);
                        (b, ev)
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }
    g.finish();
}

fn match_sweep(c: &mut Criterion) {
    let mut g = c.benchmark_group("market_order_sweep");
    for levels in [1u64, 10, 100] {
        // 10 orders per level, qty 10 each: sweeping L levels = 10*L fills.
        g.throughput(Throughput::Elements(levels * 10));
        g.bench_with_input(
            BenchmarkId::from_parameter(levels),
            &levels,
            |bch, &levels| {
                bch.iter_batched(
                    || book_with_asks(levels, 10),
                    |(mut b, id)| {
                        let ev = b.submit(OrderRequest::market(id, Side::Buy, levels * 100));
                        (b, ev)
                    },
                    BatchSize::LargeInput,
                )
            },
        );
    }
    g.finish();
}

/// A realistic-ish mix: 60% limits near the touch, 30% cancels, 10% market orders.
/// Exercises everything together, and is where cancel-heavy flow shows up.
fn mixed_flow(c: &mut Criterion) {
    let ops = mixed_ops(10_000, 42);
    let mut g = c.benchmark_group("mixed_flow");
    g.throughput(Throughput::Elements(ops.len() as u64));
    g.bench_function("10k_ops", |bch| {
        bch.iter_batched(
            OrderBook::new,
            |mut b| {
                for op in &ops {
                    match *op {
                        MixOp::Submit(r) => {
                            black_box(b.submit(r));
                        }
                        MixOp::Cancel(id) => {
                            black_box(b.cancel(id));
                        }
                    }
                }
                b
            },
            BatchSize::LargeInput,
        )
    });
    g.finish();
}

#[derive(Clone, Copy)]
enum MixOp {
    Submit(OrderRequest),
    Cancel(u64),
}

fn mixed_ops(n: usize, seed: u64) -> Vec<MixOp> {
    let mut s = seed;
    let mut rnd = move || {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        s >> 33
    };
    let mut out = Vec::with_capacity(n);
    for id in 1..=n as u64 {
        let side = if rnd() % 2 == 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        out.push(match rnd() % 10 {
            0..=5 => {
                // Bids at 990..1000, asks at 1000..1010: overlapping so some cross.
                let price = if side == Side::Buy {
                    990 + rnd() % 11
                } else {
                    1000 + rnd() % 11
                };
                MixOp::Submit(OrderRequest::limit(id, side, price, 1 + rnd() % 20))
            }
            6..=8 => MixOp::Cancel(1 + rnd() % id),
            _ => MixOp::Submit(OrderRequest::market(id, side, 1 + rnd() % 40)),
        });
    }
    out
}

criterion_group!(benches, insert, cancel, match_sweep, mixed_flow);
criterion_main!(benches);
