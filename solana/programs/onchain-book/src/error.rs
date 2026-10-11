use anchor_lang::prelude::*;

#[error_code]
pub enum BookError {
    #[msg("Price must be greater than zero")]
    ZeroPrice,
    #[msg("Quantity must be greater than zero")]
    ZeroQuantity,
    #[msg("Side must be 0 (buy) or 1 (sell)")]
    InvalidSide,
    #[msg("This side of the book is full (fixed capacity); cancel or wait for matches")]
    BookFull,
    #[msg("No resting order with that id")]
    OrderNotFound,
    #[msg("Only the order's owner can cancel it")]
    NotOwner,
    #[msg("max_fills must be between 1 and MAX_FILLS_PER_CALL")]
    InvalidMaxFills,
}
