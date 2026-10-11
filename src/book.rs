//! The order book and matching engine.
//!
//! Layout:
//!
//! ```text
//!  bids: BTreeMap<Price, Level>   ascending; best bid = LAST key
//!  asks: BTreeMap<Price, Level>   ascending; best ask = FIRST key
//!  index: HashMap<OrderId, (Side, Price, Slot)>   where each resting order lives
//!
//!  Level { orders: VecDeque<Order> (FIFO, oldest at front), head, live, total_qty }
//! ```
//!
//! M3 optimisation: O(1) cancel inside a level. Each order gets a `Slot`, its absolute
//! position in the level's queue (slot of orders[0] is `head`). Cancel jumps straight to
//! `orders[slot - head]` and turns the order into a *tombstone* (qty = 0) instead of
//! removing it from the middle of the VecDeque, which was an O(k) scan plus shift.
//! Tombstones are dropped when they reach the front, and a level compacts itself when
//! more than half its entries are dead, so memory stays bounded.
//!
//! See DESIGN.md for why these structures and the cost of every operation.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

use crate::types::*;

/// Absolute position of an order in its level's queue. Never reused within a level.
type Slot = u64;

/// Compact a level when it holds more dead entries than this AND more dead than live.
const COMPACT_MIN_DEAD: usize = 32;

/// All resting orders at one price, in arrival order.
///
/// Invariant: `orders.front()`, if any, is live (qty > 0). Tombstones (qty == 0) may sit
/// in the middle or back, never at the front, so matching never has to skip them.
#[derive(Debug, Default, Clone)]
pub struct Level {
    orders: VecDeque<Order>,
    /// Slot number of `orders[0]`.
    head: Slot,
    /// Number of live (non-tombstone) orders.
    live: usize,
    /// Cached sum of live orders' qty, so depth queries are O(1) per level.
    total_qty: Qty,
}

impl Level {
    /// Live orders, oldest first.
    pub fn orders(&self) -> impl Iterator<Item = &Order> {
        self.orders.iter().filter(|o| o.qty > 0)
    }
    pub fn total_qty(&self) -> Qty {
        self.total_qty
    }
    pub fn len(&self) -> usize {
        self.live
    }
    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    fn push(&mut self, order: Order) -> Slot {
        let slot = self.head + self.orders.len() as Slot;
        self.total_qty += order.qty;
        self.live += 1;
        self.orders.push_back(order);
        slot
    }

    fn get_mut(&mut self, slot: Slot) -> Option<&mut Order> {
        let i = slot.checked_sub(self.head)? as usize;
        self.orders.get_mut(i).filter(|o| o.qty > 0)
    }

    /// Drop the (filled) front order and any tombstones behind it.
    fn pop_front(&mut self) -> Option<Order> {
        let o = self.orders.pop_front()?;
        self.head += 1;
        self.trim_front();
        Some(o)
    }

    fn trim_front(&mut self) {
        while self.orders.front().is_some_and(|o| o.qty == 0) {
            self.orders.pop_front();
            self.head += 1;
        }
    }

    fn should_compact(&self) -> bool {
        let dead = self.orders.len() - self.live;
        dead > COMPACT_MIN_DEAD && dead > self.live
    }
}

#[derive(Debug, Default, Clone)]
pub struct OrderBook {
    bids: BTreeMap<Price, Level>,
    asks: BTreeMap<Price, Level>,
    /// Resting orders only. Filled or cancelled orders are removed.
    index: HashMap<OrderId, (Side, Price, Slot)>,
    /// Every id ever accepted, so ids can never be reused. (Grows without bound;
    /// a production engine would scope ids per session/day.)
    seen_ids: HashSet<OrderId>,
    /// Next sequence number to hand out.
    next_seq: Seq,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    // ------------------------------------------------------------------
    // Commands
    // ------------------------------------------------------------------

    /// Submit a limit or market order. Returns every event it caused, in order.
    pub fn submit(&mut self, req: OrderRequest) -> Vec<Event> {
        let mut events = Vec::new();

        // 1. Validate. Rejected orders consume no sequence number and leave no trace.
        if let Some(reason) = self.validate(&req) {
            events.push(Event::Rejected { id: req.id, reason });
            return events;
        }

        // 2. Accept and stamp with the next sequence number (our "time").
        let ts = self.next_seq;
        self.next_seq += 1;
        self.seen_ids.insert(req.id);
        events.push(Event::Accepted {
            id: req.id,
            side: req.side,
            kind: req.kind,
            qty: req.qty,
            ts,
        });

        // 3. Match against the opposite side.
        let limit = match req.kind {
            OrderKind::Limit { price } => Some(price),
            OrderKind::Market => None,
        };
        let remaining = self.match_against_book(req.id, req.side, req.qty, limit, &mut events);

        // 4. Deal with whatever is left.
        if remaining > 0 {
            match req.kind {
                OrderKind::Limit { price } => {
                    let order = Order {
                        id: req.id,
                        side: req.side,
                        price,
                        qty: remaining,
                        ts,
                    };
                    self.rest(order);
                    events.push(Event::Rested {
                        id: req.id,
                        side: req.side,
                        price,
                        qty: remaining,
                    });
                }
                OrderKind::Market => {
                    events.push(Event::Cancelled {
                        id: req.id,
                        remaining,
                        reason: CancelReason::NoLiquidity,
                    });
                }
            }
        }

        events
    }

    /// Cancel a resting order. Unknown, already-filled, or already-cancelled ids
    /// are rejected rather than silently ignored.
    ///
    /// Cost: O(1) hash lookup + O(log P) to find the level + O(1) to tombstone the order
    /// (amortised O(1) extra when the level compacts). Was O(k) before M3.
    pub fn cancel(&mut self, id: OrderId) -> Event {
        let Some((side, price, slot)) = self.index.remove(&id) else {
            return Event::Rejected {
                id,
                reason: RejectReason::UnknownOrderId,
            };
        };

        let (book, index) = match side {
            Side::Buy => (&mut self.bids, &mut self.index),
            Side::Sell => (&mut self.asks, &mut self.index),
        };
        let level = book
            .get_mut(&price)
            .expect("index points at a level that does not exist");
        let order = level
            .get_mut(slot)
            .expect("index points at a slot that is not a live order");

        // Tombstone in place: no search, no shifting.
        let remaining = order.qty;
        order.qty = 0;
        level.total_qty -= remaining;
        level.live -= 1;
        level.trim_front();

        if level.is_empty() {
            book.remove(&price);
        } else if level.should_compact() {
            Self::compact(level, index);
        }

        Event::Cancelled {
            id,
            remaining,
            reason: CancelReason::UserRequested,
        }
    }

    // ------------------------------------------------------------------
    // Queries
    // ------------------------------------------------------------------

    pub fn best_bid(&self) -> Option<Price> {
        self.bids.last_key_value().map(|(p, _)| *p)
    }

    pub fn best_ask(&self) -> Option<Price> {
        self.asks.first_key_value().map(|(p, _)| *p)
    }

    pub fn spread(&self) -> Option<Price> {
        Some(self.best_ask()? - self.best_bid()?)
    }

    /// Up to `n` levels from the top of the book on `side`: `(price, total_qty)`,
    /// best price first.
    pub fn depth(&self, side: Side, n: usize) -> Vec<(Price, Qty)> {
        match side {
            Side::Buy => self
                .bids
                .iter()
                .rev()
                .take(n)
                .map(|(p, l)| (*p, l.total_qty))
                .collect(),
            Side::Sell => self
                .asks
                .iter()
                .take(n)
                .map(|(p, l)| (*p, l.total_qty))
                .collect(),
        }
    }

    /// The level at `price` on `side`, if any. Useful for checking FIFO order.
    pub fn level(&self, side: Side, price: Price) -> Option<&Level> {
        self.side_ref(side).get(&price)
    }

    /// Look up a resting order by id. O(1) + O(log P).
    pub fn order(&self, id: OrderId) -> Option<&Order> {
        let (side, price, slot) = self.index.get(&id)?;
        let level = self.side_ref(*side).get(price)?;
        level
            .orders
            .get(slot.checked_sub(level.head)? as usize)
            .filter(|o| o.qty > 0)
    }

    /// Number of resting orders.
    pub fn len(&self) -> usize {
        self.index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    /// Sum of resting quantity on both sides.
    pub fn resting_qty(&self) -> Qty {
        self.bids
            .values()
            .chain(self.asks.values())
            .map(|l| l.total_qty)
            .sum()
    }

    /// Check every structural invariant. Panics with a message on the first
    /// violation. O(n); intended for tests (M2 will call it after every step).
    pub fn assert_invariants(&self) {
        // Not crossed.
        if let (Some(b), Some(a)) = (self.best_bid(), self.best_ask()) {
            assert!(b < a, "book is crossed: best bid {b} >= best ask {a}");
        }

        let mut counted = 0usize;
        for (side, book) in [(Side::Buy, &self.bids), (Side::Sell, &self.asks)] {
            for (price, level) in book {
                assert!(level.live > 0, "empty level left at {side:?} {price}");
                assert!(
                    level.orders.front().is_some_and(|o| o.qty > 0),
                    "tombstone at the front of {side:?} {price}"
                );
                assert_eq!(
                    level.orders().count(),
                    level.live,
                    "live count wrong at {side:?} {price}"
                );
                let sum: Qty = level.orders().map(|o| o.qty).sum();
                assert_eq!(
                    sum, level.total_qty,
                    "cached total wrong at {side:?} {price}"
                );

                let mut last_ts = None;
                for (i, o) in level.orders.iter().enumerate() {
                    if o.qty == 0 {
                        continue; // tombstone
                    }
                    assert_eq!(o.side, side, "order {} on wrong side", o.id);
                    assert_eq!(o.price, *price, "order {} at wrong level", o.id);
                    assert_eq!(
                        self.index.get(&o.id),
                        Some(&(side, *price, level.head + i as Slot)),
                        "index mismatch for {}",
                        o.id
                    );
                    if let Some(t) = last_ts {
                        assert!(o.ts > t, "FIFO violated at {side:?} {price}");
                    }
                    last_ts = Some(o.ts);
                    counted += 1;
                }
            }
        }
        assert_eq!(
            counted,
            self.index.len(),
            "index has entries for orders not in the book"
        );
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    fn validate(&self, req: &OrderRequest) -> Option<RejectReason> {
        if req.qty == 0 {
            return Some(RejectReason::ZeroQuantity);
        }
        if let OrderKind::Limit { price: 0 } = req.kind {
            return Some(RejectReason::ZeroPrice);
        }
        if self.seen_ids.contains(&req.id) {
            return Some(RejectReason::DuplicateOrderId);
        }
        None
    }

    /// Walk the opposite side from the best price outwards, filling `qty`.
    /// Stops when filled, when the side is empty, or when the next price is
    /// worse than `limit`. Returns the unfilled quantity.
    fn match_against_book(
        &mut self,
        taker_id: OrderId,
        taker_side: Side,
        mut qty: Qty,
        limit: Option<Price>,
        events: &mut Vec<Event>,
    ) -> Qty {
        // Split borrows: we need the opposite side's map and the index at once.
        let (book, index) = match taker_side {
            Side::Buy => (&mut self.asks, &mut self.index),
            Side::Sell => (&mut self.bids, &mut self.index),
        };

        while qty > 0 {
            // Best opposing price: lowest ask for a buyer, highest bid for a seller.
            let best = match taker_side {
                Side::Buy => book.first_key_value(),
                Side::Sell => book.last_key_value(),
            };
            let Some((&price, _)) = best else { break }; // side is empty

            // Does the taker's limit allow trading at this price?
            if let Some(lim) = limit {
                let crosses = match taker_side {
                    Side::Buy => price <= lim,
                    Side::Sell => price >= lim,
                };
                if !crosses {
                    break;
                }
            }

            let level = book.get_mut(&price).expect("key just observed");

            // Fill against this level, oldest order first. The front is always live.
            while qty > 0 {
                let Some(maker) = level.orders.front_mut() else {
                    break;
                };
                let fill = qty.min(maker.qty);

                maker.qty -= fill;
                level.total_qty -= fill;
                qty -= fill;

                events.push(Event::Trade(Trade {
                    maker_id: maker.id,
                    taker_id,
                    taker_side,
                    price, // trades print at the maker's price
                    qty: fill,
                }));

                if maker.qty == 0 {
                    let done_id = maker.id;
                    level.live -= 1;
                    level.pop_front(); // also drops tombstones that are now at the front
                    index.remove(&done_id);
                }
            }

            if level.is_empty() {
                book.remove(&price);
            }
        }

        qty
    }

    fn rest(&mut self, order: Order) {
        let (id, side, price) = (order.id, order.side, order.price);
        let slot = self.side_mut(side).entry(price).or_default().push(order);
        self.index.insert(id, (side, price, slot));
    }

    /// Remove tombstones from a level and renumber the live orders' slots. O(k), but only
    /// runs once dead entries outnumber live ones, so it is amortised O(1) per cancel.
    fn compact(level: &mut Level, index: &mut HashMap<OrderId, (Side, Price, Slot)>) {
        level.orders.retain(|o| o.qty > 0);
        for (i, o) in level.orders.iter().enumerate() {
            if let Some(entry) = index.get_mut(&o.id) {
                entry.2 = level.head + i as Slot;
            }
        }
    }

    fn side_mut(&mut self, side: Side) -> &mut BTreeMap<Price, Level> {
        match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        }
    }

    fn side_ref(&self, side: Side) -> &BTreeMap<Price, Level> {
        match side {
            Side::Buy => &self.bids,
            Side::Sell => &self.asks,
        }
    }
}
