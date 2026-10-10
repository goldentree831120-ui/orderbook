//! Scenario tests for the M1 engine. Each test checks one rule.
//! (M2 adds property-based tests on top of these.)

use orderbook::*;

// ---------- helpers ----------

fn trades(events: &[Event]) -> Vec<Trade> {
    events
        .iter()
        .filter_map(|e| {
            if let Event::Trade(t) = e {
                Some(*t)
            } else {
                None
            }
        })
        .collect()
}

fn t(maker: OrderId, taker: OrderId, side: Side, price: Price, qty: Qty) -> Trade {
    Trade {
        maker_id: maker,
        taker_id: taker,
        taker_side: side,
        price,
        qty,
    }
}

fn ids_at(book: &OrderBook, side: Side, price: Price) -> Vec<OrderId> {
    book.level(side, price)
        .map(|l| l.orders().map(|o| o.id).collect())
        .unwrap_or_default()
}

/// Small deterministic RNG so the stress test needs no dependencies.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

// ---------- resting & basic matching ----------

#[test]
fn non_crossing_limits_rest_on_both_sides() {
    let mut b = OrderBook::new();
    let e1 = b.submit(OrderRequest::limit(1, Side::Buy, 99, 10));
    let e2 = b.submit(OrderRequest::limit(2, Side::Sell, 101, 5));

    assert!(trades(&e1).is_empty() && trades(&e2).is_empty());
    assert_eq!(b.best_bid(), Some(99));
    assert_eq!(b.best_ask(), Some(101));
    assert_eq!(b.spread(), Some(2));
    assert_eq!(b.len(), 2);
    b.assert_invariants();
}

#[test]
fn event_sequence_for_a_partial_fill() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 3));
    let ev = b.submit(OrderRequest::limit(2, Side::Buy, 100, 5));

    assert_eq!(
        ev,
        vec![
            Event::Accepted {
                id: 2,
                side: Side::Buy,
                kind: OrderKind::Limit { price: 100 },
                qty: 5,
                ts: 1
            },
            Event::Trade(t(1, 2, Side::Buy, 100, 3)),
            Event::Rested {
                id: 2,
                side: Side::Buy,
                price: 100,
                qty: 2
            },
        ]
    );
    assert_eq!(b.best_ask(), None);
    assert_eq!(b.depth(Side::Buy, 5), vec![(100, 2)]);
    b.assert_invariants();
}

#[test]
fn trade_prints_at_maker_price_not_taker_price() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 10));
    // Buyer is willing to pay 105 but gets the resting 100.
    let ev = b.submit(OrderRequest::limit(2, Side::Buy, 105, 10));
    assert_eq!(trades(&ev), vec![t(1, 2, Side::Buy, 100, 10)]);
    assert!(b.is_empty());
}

#[test]
fn maker_partially_filled_keeps_its_queue_position() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 10));
    b.submit(OrderRequest::limit(2, Side::Sell, 100, 10));
    b.submit(OrderRequest::market(3, Side::Buy, 4));

    assert_eq!(ids_at(&b, Side::Sell, 100), vec![1, 2]);
    assert_eq!(b.order(1).unwrap().qty, 6);
    assert_eq!(
        b.order(1).unwrap().ts,
        0,
        "partial fill must not reset priority"
    );
    b.assert_invariants();
}

// ---------- price-time priority ----------

#[test]
fn better_price_fills_first_regardless_of_arrival() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 102, 5)); // earlier, worse price
    b.submit(OrderRequest::limit(2, Side::Sell, 101, 5)); // later, better price
    let ev = b.submit(OrderRequest::limit(3, Side::Buy, 102, 5));
    assert_eq!(trades(&ev), vec![t(2, 3, Side::Buy, 101, 5)]);
}

#[test]
fn fifo_within_a_price_level() {
    let mut b = OrderBook::new();
    for id in 1..=3 {
        b.submit(OrderRequest::limit(id, Side::Buy, 100, 2));
    }
    let ev = b.submit(OrderRequest::market(9, Side::Sell, 5));
    assert_eq!(
        trades(&ev),
        vec![
            t(1, 9, Side::Sell, 100, 2),
            t(2, 9, Side::Sell, 100, 2),
            t(3, 9, Side::Sell, 100, 1)
        ]
    );
    assert_eq!(ids_at(&b, Side::Buy, 100), vec![3]);
    assert_eq!(b.order(3).unwrap().qty, 1);
}

#[test]
fn buy_sweeps_several_ask_levels_and_rests_remainder() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 2));
    b.submit(OrderRequest::limit(2, Side::Sell, 101, 3));
    b.submit(OrderRequest::limit(3, Side::Sell, 103, 4)); // beyond limit

    let ev = b.submit(OrderRequest::limit(4, Side::Buy, 102, 10));
    assert_eq!(
        trades(&ev),
        vec![t(1, 4, Side::Buy, 100, 2), t(2, 4, Side::Buy, 101, 3)]
    );
    assert_eq!(b.best_bid(), Some(102));
    assert_eq!(b.depth(Side::Buy, 1), vec![(102, 5)]);
    assert_eq!(b.best_ask(), Some(103));
    b.assert_invariants();
}

#[test]
fn sell_sweeps_bid_levels_from_highest_down() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Buy, 98, 5));
    b.submit(OrderRequest::limit(2, Side::Buy, 100, 5));
    b.submit(OrderRequest::limit(3, Side::Buy, 99, 5));

    let ev = b.submit(OrderRequest::limit(4, Side::Sell, 99, 12));
    assert_eq!(
        trades(&ev),
        vec![t(2, 4, Side::Sell, 100, 5), t(3, 4, Side::Sell, 99, 5)]
    );
    // 2 left over at 99, which is above the remaining bid at 98: rests as the best ask.
    assert_eq!(b.best_ask(), Some(99));
    assert_eq!(b.best_bid(), Some(98));
    b.assert_invariants();
}

#[test]
fn emptied_levels_are_removed() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 1));
    b.submit(OrderRequest::limit(2, Side::Sell, 101, 1));
    b.submit(OrderRequest::market(3, Side::Buy, 1));
    assert!(b.level(Side::Sell, 100).is_none());
    assert_eq!(b.best_ask(), Some(101));
    b.assert_invariants();
}

// ---------- market orders ----------

#[test]
fn market_order_remainder_is_cancelled_not_rested() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 3));
    let ev = b.submit(OrderRequest::market(2, Side::Buy, 10));

    assert_eq!(trades(&ev), vec![t(1, 2, Side::Buy, 100, 3)]);
    assert_eq!(
        ev.last(),
        Some(&Event::Cancelled {
            id: 2,
            remaining: 7,
            reason: CancelReason::NoLiquidity
        })
    );
    assert!(b.is_empty());
}

#[test]
fn market_order_on_empty_book() {
    let mut b = OrderBook::new();
    let ev = b.submit(OrderRequest::market(1, Side::Sell, 5));
    assert_eq!(ev.len(), 2);
    assert!(matches!(
        ev[1],
        Event::Cancelled {
            remaining: 5,
            reason: CancelReason::NoLiquidity,
            ..
        }
    ));
}

// ---------- cancel ----------

#[test]
fn cancel_removes_order_and_reports_remaining() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Buy, 100, 10));
    b.submit(OrderRequest::market(2, Side::Sell, 3));

    assert_eq!(
        b.cancel(1),
        Event::Cancelled {
            id: 1,
            remaining: 7,
            reason: CancelReason::UserRequested
        }
    );
    assert!(b.is_empty());
    assert_eq!(b.best_bid(), None);
    b.assert_invariants();
}

#[test]
fn cancel_from_middle_of_queue_preserves_others_order() {
    let mut b = OrderBook::new();
    for id in 1..=4 {
        b.submit(OrderRequest::limit(id, Side::Sell, 100, 1));
    }
    b.cancel(2);
    assert_eq!(ids_at(&b, Side::Sell, 100), vec![1, 3, 4]);
    assert_eq!(b.level(Side::Sell, 100).unwrap().total_qty(), 3);
    b.assert_invariants();
}

#[test]
fn cancel_unknown_filled_or_twice_is_rejected() {
    let mut b = OrderBook::new();
    let unknown = Event::Rejected {
        id: 42,
        reason: RejectReason::UnknownOrderId,
    };
    assert_eq!(b.cancel(42), unknown);

    b.submit(OrderRequest::limit(1, Side::Sell, 100, 1));
    b.submit(OrderRequest::market(2, Side::Buy, 1)); // fully fills order 1
    assert_eq!(
        b.cancel(1),
        Event::Rejected {
            id: 1,
            reason: RejectReason::UnknownOrderId
        }
    );

    b.submit(OrderRequest::limit(3, Side::Sell, 100, 1));
    assert!(matches!(b.cancel(3), Event::Cancelled { .. }));
    assert_eq!(
        b.cancel(3),
        Event::Rejected {
            id: 3,
            reason: RejectReason::UnknownOrderId
        }
    );
}

// ---------- validation ----------

#[test]
fn invalid_requests_are_rejected_and_do_not_consume_a_sequence_number() {
    let mut b = OrderBook::new();
    assert_eq!(
        b.submit(OrderRequest::limit(1, Side::Buy, 100, 0)),
        vec![Event::Rejected {
            id: 1,
            reason: RejectReason::ZeroQuantity
        }]
    );
    assert_eq!(
        b.submit(OrderRequest::limit(1, Side::Buy, 0, 5)),
        vec![Event::Rejected {
            id: 1,
            reason: RejectReason::ZeroPrice
        }]
    );
    b.submit(OrderRequest::limit(1, Side::Buy, 100, 5));
    assert_eq!(b.order(1).unwrap().ts, 0);
    assert_eq!(
        b.submit(OrderRequest::limit(1, Side::Sell, 200, 5)),
        vec![Event::Rejected {
            id: 1,
            reason: RejectReason::DuplicateOrderId
        }]
    );
}

#[test]
fn ids_of_fully_filled_orders_cannot_be_reused() {
    let mut b = OrderBook::new();
    b.submit(OrderRequest::limit(1, Side::Sell, 100, 1));
    b.submit(OrderRequest::market(2, Side::Buy, 1));
    assert_eq!(
        b.submit(OrderRequest::limit(1, Side::Sell, 100, 1)),
        vec![Event::Rejected {
            id: 1,
            reason: RejectReason::DuplicateOrderId
        }]
    );
}

// ---------- stress: invariants + conservation over a random stream ----------

#[test]
fn random_stream_keeps_invariants_and_conserves_quantity() {
    let mut rng = Lcg(7);
    let mut b = OrderBook::new();
    let (mut submitted, mut filled_x2, mut cancelled) = (0u64, 0u64, 0u64);
    let mut live: Vec<OrderId> = Vec::new();

    for id in 1..=5_000u64 {
        let side = if rng.next().is_multiple_of(2) {
            Side::Buy
        } else {
            Side::Sell
        };
        let qty = rng.range(1, 50);
        let ev = match rng.next() % 10 {
            0..=5 => b.submit(OrderRequest::limit(id, side, rng.range(95, 105), qty)),
            6 => b.submit(OrderRequest::market(id, side, qty)),
            _ if !live.is_empty() => {
                let victim = live.swap_remove((rng.next() as usize) % live.len());
                vec![b.cancel(victim)]
            }
            _ => continue,
        };
        for e in &ev {
            match e {
                Event::Accepted { qty, .. } => submitted += qty,
                // A trade fills both the maker and the taker by `qty`.
                Event::Trade(t) => filled_x2 += 2 * t.qty,
                Event::Cancelled { remaining, .. } => cancelled += remaining,
                Event::Rested { id, .. } => live.push(*id),
                Event::Rejected { .. } => {}
            }
        }
        b.assert_invariants();
        assert_eq!(
            submitted,
            filled_x2 + b.resting_qty() + cancelled,
            "quantity leaked at step {id}"
        );
    }
}
