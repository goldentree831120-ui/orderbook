use anchor_lang::prelude::*;

#[event]
pub struct OrderPlaced {
    pub order_id: u64,
    pub owner: Pubkey,
    pub side: u8,
    pub price: u64,
    pub qty: u64,
}

#[event]
pub struct OrderCancelled {
    pub order_id: u64,
    pub remaining: u64,
}

#[event]
pub struct TradeEvent {
    pub maker_id: u64,
    pub taker_id: u64,
    pub taker_side: u8,
    pub price: u64,
    pub qty: u64,
}

/// Emitted at the end of every match_orders call.
#[event]
pub struct MatchSummary {
    pub fills: u16,
    /// True if the book still crosses (more fills pending): call match_orders again.
    pub still_crossed: bool,
}
