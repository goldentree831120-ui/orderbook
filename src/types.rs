//! Plain data types shared by the engine.
//!
//! Prices and quantities are integers. A `Price` is a number of ticks
//! (e.g. 1 tick = $0.01), a `Qty` is a number of lots. Floats are never used:
//! `0.1 + 0.2 != 0.3` would make "is the book crossed?" and
//! "is this level empty?" unreliable, and would break determinism (M4).

/// Client-chosen order identifier. Must be unique for the lifetime of the book.
pub type OrderId = u64;
/// Price in ticks. Always > 0 for limit orders.
pub type Price = u64;
/// Quantity in lots.
pub type Qty = u64;
/// Engine-assigned sequence number. Strictly increasing, one per accepted order.
/// This is the "time" in price-time priority; it never comes from a wall clock.
pub type Seq = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }
}

/// What the caller asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderKind {
    /// Match up to `price`, rest any remainder on the book.
    Limit { price: Price },
    /// Match at any price; any remainder is cancelled (never rests).
    Market,
}

/// An incoming instruction to add liquidity or take it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrderRequest {
    pub id: OrderId,
    pub side: Side,
    pub kind: OrderKind,
    pub qty: Qty,
}

impl OrderRequest {
    pub fn limit(id: OrderId, side: Side, price: Price, qty: Qty) -> Self {
        Self {
            id,
            side,
            kind: OrderKind::Limit { price },
            qty,
        }
    }
    pub fn market(id: OrderId, side: Side, qty: Qty) -> Self {
        Self {
            id,
            side,
            kind: OrderKind::Market,
            qty,
        }
    }
}

/// A resting order as stored in the book. `qty` is the *remaining* quantity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: OrderId,
    pub side: Side,
    pub price: Price,
    pub qty: Qty,
    pub ts: Seq,
}

/// One execution between a resting (maker) order and an incoming (taker) order.
/// It always prints at the maker's price.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trade {
    pub maker_id: OrderId,
    pub taker_id: OrderId,
    pub taker_side: Side,
    pub price: Price,
    pub qty: Qty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    /// The client asked for it.
    UserRequested,
    /// A market order ran out of opposite liquidity before it was fully filled.
    NoLiquidity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    ZeroQuantity,
    ZeroPrice,
    DuplicateOrderId,
    UnknownOrderId,
}

/// Everything the engine does is reported as an ordered list of events.
/// A downstream consumer (market data feed, M4 replay log, M2 property tests)
/// can reconstruct the book from these alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Accepted {
        id: OrderId,
        side: Side,
        kind: OrderKind,
        qty: Qty,
        ts: Seq,
    },
    Trade(Trade),
    /// The unfilled remainder of a limit order was placed on the book.
    Rested {
        id: OrderId,
        side: Side,
        price: Price,
        qty: Qty,
    },
    /// `remaining` is the quantity removed from the book (or never placed, for market orders).
    Cancelled {
        id: OrderId,
        remaining: Qty,
        reason: CancelReason,
    },
    Rejected {
        id: OrderId,
        reason: RejectReason,
    },
}
