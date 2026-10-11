//! The market account and the pure order-book logic.
//!
//! Everything in `impl Market` is plain Rust with no Anchor context, so the same code runs
//! on-chain and in native unit tests.
//!
//! Layout: two fixed arrays of `CAPACITY` orders, each kept SORTED best-first:
//!   bids: highest price first, then oldest (lowest id) first
//!   asks: lowest price first, then oldest first
//! so the best order of each side is always at index 0 and price-time priority is just
//! array order. Order ids are assigned on-chain from a counter, so a lower id is always an
//! older order: the id doubles as the time-priority sequence number.

use anchor_lang::prelude::*;

use crate::error::BookError;

/// Orders per side. 2 x 64 x 64 bytes = 8 KiB of orders; the whole account is 8,280 bytes,
/// under the 10,240-byte limit for an account created by a program (CPI) in one instruction.
pub const CAPACITY: usize = 64;

/// Upper bound on fills per `match_orders` call, so one call can never exceed the compute
/// budget. Chosen from the measured cost per fill (see tests/compute_units.rs and README).
pub const MAX_FILLS_PER_CALL: u16 = 32;

pub const SIDE_BUY: u8 = 0;
pub const SIDE_SELL: u8 = 1;

#[zero_copy]
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: u64,
    pub price: u64,
    pub qty: u64,
    pub _reserved: u64,
    pub owner: Pubkey,
}

/// Zero-copy: the 8 KiB of orders are read and written in place in the account data,
/// never deserialised onto the 4 KiB stack (which a normal `#[account]` would try to do).
#[account(zero_copy)]
pub struct Market {
    pub authority: Pubkey,
    pub next_order_id: u64,
    pub trade_count: u64,
    pub traded_qty: u64,
    pub bid_len: u64,
    pub ask_len: u64,
    pub bump: u64,
    pub bids: [Order; CAPACITY],
    pub asks: [Order; CAPACITY],
}

/// One execution, returned by `match_step` and emitted as an event by the instruction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub maker_id: u64,
    pub taker_id: u64,
    pub taker_side: u8,
    pub price: u64,
    pub qty: u64,
    pub maker_remaining: u64,
    pub taker_remaining: u64,
}

impl Market {
    pub const SPACE: usize = 8 + core::mem::size_of::<Market>();

    pub fn bids(&self) -> &[Order] {
        &self.bids[..self.bid_len as usize]
    }

    pub fn asks(&self) -> &[Order] {
        &self.asks[..self.ask_len as usize]
    }

    /// Rest a new order in price-time position. O(N) shift. Returns the new order id.
    /// Matching is NOT done here: crossing orders rest until `match_step` runs.
    pub fn insert(&mut self, side: u8, price: u64, qty: u64, owner: Pubkey) -> Result<u64> {
        require!(price > 0, BookError::ZeroPrice);
        require!(qty > 0, BookError::ZeroQuantity);
        let (arr, len) = match side {
            SIDE_BUY => (&mut self.bids, &mut self.bid_len),
            SIDE_SELL => (&mut self.asks, &mut self.ask_len),
            _ => return err!(BookError::InvalidSide),
        };
        let n = *len as usize;
        require!(n < CAPACITY, BookError::BookFull);

        self.next_order_id += 1;
        let id = self.next_order_id;
        // First index whose order has WORSE price than the new one. Equal price: the new order
        // goes behind existing ones (it is newer), which is FIFO within the level.
        let pos = arr[..n]
            .iter()
            .position(|o| {
                if side == SIDE_BUY {
                    o.price < price
                } else {
                    o.price > price
                }
            })
            .unwrap_or(n);
        arr.copy_within(pos..n, pos + 1);
        arr[pos] = Order {
            id,
            price,
            qty,
            _reserved: 0,
            owner,
        };
        *len += 1;
        Ok(id)
    }

    /// Remove order `id` if `owner` owns it. O(N) scan + shift. Returns its remaining qty.
    pub fn cancel(&mut self, id: u64, owner: &Pubkey) -> Result<u64> {
        for (arr, len) in [
            (&mut self.bids, &mut self.bid_len),
            (&mut self.asks, &mut self.ask_len),
        ] {
            let n = *len as usize;
            if let Some(i) = arr[..n].iter().position(|o| o.id == id) {
                require_keys_eq!(arr[i].owner, *owner, BookError::NotOwner);
                let qty = arr[i].qty;
                arr.copy_within(i + 1..n, i);
                arr[n - 1] = Order::default();
                *len -= 1;
                return Ok(qty);
            }
        }
        err!(BookError::OrderNotFound)
    }

    /// Execute at most one fill between the best bid and best ask, if they cross.
    /// The older order (lower id) is the maker and sets the price.
    pub fn match_step(&mut self) -> Option<Fill> {
        if self.bid_len == 0 || self.ask_len == 0 || self.bids[0].price < self.asks[0].price {
            return None;
        }
        let (bid, ask) = (self.bids[0], self.asks[0]);
        let qty = bid.qty.min(ask.qty);
        let (maker, taker, taker_side) = if bid.id < ask.id {
            (bid, ask, SIDE_SELL)
        } else {
            (ask, bid, SIDE_BUY)
        };

        self.bids[0].qty -= qty;
        self.asks[0].qty -= qty;
        let fill = Fill {
            maker_id: maker.id,
            taker_id: taker.id,
            taker_side,
            price: maker.price,
            qty,
            maker_remaining: maker.qty - qty,
            taker_remaining: taker.qty - qty,
        };
        if self.bids[0].qty == 0 {
            let n = self.bid_len as usize;
            self.bids.copy_within(1..n, 0);
            self.bids[n - 1] = Order::default();
            self.bid_len -= 1;
        }
        if self.asks[0].qty == 0 {
            let n = self.ask_len as usize;
            self.asks.copy_within(1..n, 0);
            self.asks[n - 1] = Order::default();
            self.ask_len -= 1;
        }
        self.trade_count += 1;
        self.traded_qty += qty;
        Some(fill)
    }

    /// True if the best bid and best ask cross (matching work is pending).
    pub fn is_crossed(&self) -> bool {
        self.bid_len > 0 && self.ask_len > 0 && self.bids[0].price >= self.asks[0].price
    }
}
