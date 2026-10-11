//! M2: property-based tests.
//!
//! proptest generates thousands of random sequences of limit orders, market orders,
//! cancels and invalid requests, runs them through the engine, and after EVERY step checks:
//!
//! 1. the book is never crossed (best bid < best ask);
//! 2. quantity is conserved, globally and for each individual order:
//!    submitted = filled + resting + cancelled
//! 3. FIFO within a price level, plus structural consistency (OrderBook::assert_invariants);
//! 4. the engine produces exactly the same events as a deliberately naive reference
//!    implementation (differential testing). The reference finds the best order by scanning
//!    a Vec, so it is slow but easy to check by eye; if the BTreeMap/VecDeque engine ever
//!    disagrees with it, the engine has a bug in price-time priority.
//!
//! When a property fails, proptest shrinks the input to the smallest failing sequence.

use std::collections::HashMap;

use orderbook::*;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Operation generator
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    Limit {
        side: Side,
        price: Price,
        qty: Qty,
    },
    Market {
        side: Side,
        qty: Qty,
    },
    /// Cancel the n-th id ever submitted (may be resting, filled, cancelled or rejected).
    Cancel {
        pick: usize,
    },
    /// Re-use an id that was already used: must be rejected.
    Duplicate {
        pick: usize,
    },
    ZeroQty {
        side: Side,
    },
}

fn side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Buy), Just(Side::Sell)]
}

fn op() -> impl Strategy<Value = Op> {
    // A narrow price band (95..=105) makes orders cross often, which is where bugs live.
    prop_oneof![
        6 => (side(), 95u64..=105, 1u64..=50).prop_map(|(side, price, qty)| Op::Limit { side, price, qty }),
        2 => (side(), 1u64..=120).prop_map(|(side, qty)| Op::Market { side, qty }),
        3 => any::<usize>().prop_map(|pick| Op::Cancel { pick }),
        1 => any::<usize>().prop_map(|pick| Op::Duplicate { pick }),
        1 => side().prop_map(|side| Op::ZeroQty { side }),
    ]
}

// ---------------------------------------------------------------------------
// Reference implementation: obviously-correct, O(n) per operation
// ---------------------------------------------------------------------------

#[derive(Default)]
struct RefBook {
    resting: Vec<Order>, // both sides, arrival order
    seen: Vec<OrderId>,
    seq: Seq,
}

impl RefBook {
    fn submit(&mut self, req: OrderRequest) -> Vec<Event> {
        if req.qty == 0 {
            return vec![Event::Rejected {
                id: req.id,
                reason: RejectReason::ZeroQuantity,
            }];
        }
        if let OrderKind::Limit { price: 0 } = req.kind {
            return vec![Event::Rejected {
                id: req.id,
                reason: RejectReason::ZeroPrice,
            }];
        }
        if self.seen.contains(&req.id) {
            return vec![Event::Rejected {
                id: req.id,
                reason: RejectReason::DuplicateOrderId,
            }];
        }
        self.seen.push(req.id);
        let ts = self.seq;
        self.seq += 1;
        let mut ev = vec![Event::Accepted {
            id: req.id,
            side: req.side,
            kind: req.kind,
            qty: req.qty,
            ts,
        }];
        let limit = match req.kind {
            OrderKind::Limit { price } => Some(price),
            OrderKind::Market => None,
        };
        let mut left = req.qty;
        while left > 0 {
            // Best maker: best price for the taker, then earliest ts. Plain linear scan.
            let best = self
                .resting
                .iter()
                .enumerate()
                .filter(|(_, o)| o.side != req.side)
                .filter(|(_, o)| match (req.side, limit) {
                    (_, None) => true,
                    (Side::Buy, Some(l)) => o.price <= l,
                    (Side::Sell, Some(l)) => o.price >= l,
                })
                .min_by_key(|(_, o)| {
                    let price_rank = if req.side == Side::Buy {
                        o.price as i128
                    } else {
                        -(o.price as i128)
                    };
                    (price_rank, o.ts)
                })
                .map(|(i, _)| i);
            let Some(i) = best else { break };
            let fill = left.min(self.resting[i].qty);
            let maker = &mut self.resting[i];
            maker.qty -= fill;
            left -= fill;
            ev.push(Event::Trade(Trade {
                maker_id: maker.id,
                taker_id: req.id,
                taker_side: req.side,
                price: maker.price,
                qty: fill,
            }));
            if maker.qty == 0 {
                self.resting.remove(i);
            }
        }
        if left > 0 {
            match req.kind {
                OrderKind::Limit { price } => {
                    self.resting.push(Order {
                        id: req.id,
                        side: req.side,
                        price,
                        qty: left,
                        ts,
                    });
                    ev.push(Event::Rested {
                        id: req.id,
                        side: req.side,
                        price,
                        qty: left,
                    });
                }
                OrderKind::Market => ev.push(Event::Cancelled {
                    id: req.id,
                    remaining: left,
                    reason: CancelReason::NoLiquidity,
                }),
            }
        }
        ev
    }

    fn cancel(&mut self, id: OrderId) -> Event {
        match self.resting.iter().position(|o| o.id == id) {
            Some(i) => {
                let o = self.resting.remove(i);
                Event::Cancelled {
                    id,
                    remaining: o.qty,
                    reason: CancelReason::UserRequested,
                }
            }
            None => Event::Rejected {
                id,
                reason: RejectReason::UnknownOrderId,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Per-order bookkeeping from the event stream alone
// ---------------------------------------------------------------------------

#[derive(Default, Debug, Clone, Copy)]
struct Ledger {
    submitted: Qty,
    filled: Qty,
    cancelled: Qty,
}

fn record(ledgers: &mut HashMap<OrderId, Ledger>, events: &[Event]) {
    for e in events {
        match *e {
            Event::Accepted { id, qty, .. } => ledgers.entry(id).or_default().submitted += qty,
            Event::Trade(t) => {
                ledgers.entry(t.maker_id).or_default().filled += t.qty;
                ledgers.entry(t.taker_id).or_default().filled += t.qty;
            }
            Event::Cancelled { id, remaining, .. } => {
                ledgers.entry(id).or_default().cancelled += remaining
            }
            Event::Rested { .. } | Event::Rejected { .. } => {}
        }
    }
}

/// Runs `ops` through both books and checks every property after every step.
fn check_sequence(ops: &[Op]) -> Result<(), TestCaseError> {
    let mut book = OrderBook::new();
    let mut reference = RefBook::default();
    let mut ledgers: HashMap<OrderId, Ledger> = HashMap::new();
    let mut ids: Vec<OrderId> = Vec::new();
    let mut next_id: OrderId = 1;

    for (step, op) in ops.iter().enumerate() {
        let (events, expected) = match *op {
            Op::Limit { side, price, qty } => {
                let req = OrderRequest::limit(next_id, side, price, qty);
                ids.push(next_id);
                next_id += 1;
                (book.submit(req), reference.submit(req))
            }
            Op::Market { side, qty } => {
                let req = OrderRequest::market(next_id, side, qty);
                ids.push(next_id);
                next_id += 1;
                (book.submit(req), reference.submit(req))
            }
            Op::ZeroQty { side } => {
                let req = OrderRequest::limit(next_id, side, 100, 0);
                next_id += 1;
                (book.submit(req), reference.submit(req))
            }
            Op::Cancel { pick } => {
                let id = if ids.is_empty() {
                    999_999
                } else {
                    ids[pick % ids.len()]
                };
                (vec![book.cancel(id)], vec![reference.cancel(id)])
            }
            Op::Duplicate { pick } => {
                if ids.is_empty() {
                    continue;
                }
                let req = OrderRequest::limit(ids[pick % ids.len()], Side::Buy, 100, 1);
                let ev = book.submit(req);
                let rejected = matches!(
                    ev[..],
                    [Event::Rejected {
                        reason: RejectReason::DuplicateOrderId,
                        ..
                    }]
                );
                prop_assert!(rejected, "duplicate id was not rejected: {:?}", ev);
                (ev, reference.submit(req))
            }
        };

        // (4) differential: identical events, in identical order
        prop_assert_eq!(
            &events,
            &expected,
            "engine and reference disagree at step {} ({:?})",
            step,
            op
        );

        // (1) + (3): not crossed, FIFO, cached totals, index consistent
        book.assert_invariants();

        // (2) conservation, per order and in total
        record(&mut ledgers, &events);
        let mut total_in = 0;
        let mut total_out = 0;
        for (id, l) in &ledgers {
            let resting = book.order(*id).map_or(0, |o| o.qty);
            prop_assert_eq!(
                l.submitted,
                l.filled + resting + l.cancelled,
                "order {} leaked quantity at step {}: {:?}, resting {}",
                id,
                step,
                l,
                resting
            );
            total_in += l.submitted;
            total_out += l.filled + l.cancelled;
        }
        prop_assert_eq!(total_in, total_out + book.resting_qty());
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2_000, ..ProptestConfig::default() })]

    #[test]
    fn random_sequences_hold_every_invariant(ops in prop::collection::vec(op(), 1..200)) {
        check_sequence(&ops)?;
    }

    /// Long-ish sequences on one side of the market then a sweep: stresses deep queues.
    #[test]
    fn deep_queues_then_sweep(
        n in 1usize..60, qty in 1u64..20, sweep in 1u64..2_000, side in side()
    ) {
        let mut ops: Vec<Op> = (0..n).map(|i| Op::Limit { side, price: 100 + (i as u64 % 3), qty }).collect();
        ops.push(Op::Market { side: side.opposite(), qty: sweep });
        check_sequence(&ops)?;
    }

    /// A trade always prints at the maker's price, and never outside the taker's limit.
    #[test]
    fn trades_respect_limit_and_print_at_maker_price(ops in prop::collection::vec(op(), 1..150)) {
        let mut book = OrderBook::new();
        let mut next_id = 1;
        for op in ops {
            if let Op::Limit { side, price, qty } = op {
                for e in book.submit(OrderRequest::limit(next_id, side, price, qty)) {
                    if let Event::Trade(t) = e {
                        match side {
                            Side::Buy => prop_assert!(t.price <= price),
                            Side::Sell => prop_assert!(t.price >= price),
                        }
                        prop_assert_ne!(t.maker_id, next_id);
                    }
                }
                next_id += 1;
            }
        }
    }
}

/// Sanity check that the reference model is itself right on a hand-worked example,
/// so the differential test is comparing against something trustworthy.
#[test]
fn reference_model_matches_hand_worked_example() {
    let mut r = RefBook::default();
    r.submit(OrderRequest::limit(1, Side::Sell, 101, 5));
    r.submit(OrderRequest::limit(2, Side::Sell, 102, 3));
    r.submit(OrderRequest::limit(3, Side::Sell, 101, 2));
    let ev = r.submit(OrderRequest::limit(4, Side::Buy, 102, 9));
    let trades: Vec<_> = ev
        .iter()
        .filter_map(|e| {
            if let Event::Trade(t) = e {
                Some((t.maker_id, t.price, t.qty))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(trades, vec![(1, 101, 5), (3, 101, 2), (2, 102, 2)]);
}
