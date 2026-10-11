//! P1 M5: a price-time priority order book as a Solana program.
//!
//! One `Market` account holds both sides of the book in fixed-size arrays. Three user
//! instructions mirror the Rust engine (M1): place_order, cancel_order, and match_orders.
//! Matching is a separate, permissionless "crank" so every instruction has a bounded cost.
//! README.md explains the account-size and compute limits behind each choice.

pub mod error;
pub mod events;
pub mod state;

use anchor_lang::prelude::*;

pub use error::BookError;
pub use events::*;
pub use state::*;

declare_id!("AgJTtauLP5QiYBLEvZGrnyPsu2mwb85Eo18d6rZ2Gg37");

pub const MARKET_SEED: &[u8] = b"market";

#[program]
pub mod onchain_book {
    use super::*;

    /// Create the market account (a PDA of the authority, so one market per authority).
    pub fn initialize_market(ctx: Context<InitializeMarket>) -> Result<()> {
        let mut m = ctx.accounts.market.load_init()?;
        m.authority = ctx.accounts.authority.key();
        m.bump = ctx.bumps.market as u64;
        Ok(())
    }

    /// Rest a limit order. side: 0 = buy, 1 = sell. Does not match: call match_orders.
    pub fn place_order(ctx: Context<PlaceOrder>, side: u8, price: u64, qty: u64) -> Result<()> {
        let owner = ctx.accounts.owner.key();
        let mut m = ctx.accounts.market.load_mut()?;
        let order_id = m.insert(side, price, qty, owner)?;
        emit!(OrderPlaced {
            order_id,
            owner,
            side,
            price,
            qty
        });
        Ok(())
    }

    /// Cancel one of your own resting orders.
    pub fn cancel_order(ctx: Context<CancelOrder>, order_id: u64) -> Result<()> {
        let mut m = ctx.accounts.market.load_mut()?;
        let remaining = m.cancel(order_id, &ctx.accounts.owner.key())?;
        emit!(OrderCancelled {
            order_id,
            remaining
        });
        Ok(())
    }

    /// Match crossing orders, at most `max_fills` fills. Anyone may call it.
    pub fn match_orders(ctx: Context<MatchOrders>, max_fills: u16) -> Result<()> {
        require!(
            (1..=MAX_FILLS_PER_CALL).contains(&max_fills),
            BookError::InvalidMaxFills
        );
        let mut m = ctx.accounts.market.load_mut()?;
        let mut fills = 0u16;
        while fills < max_fills {
            let Some(f) = m.match_step() else { break };
            emit!(TradeEvent {
                maker_id: f.maker_id,
                taker_id: f.taker_id,
                taker_side: f.taker_side,
                price: f.price,
                qty: f.qty
            });
            fills += 1;
        }
        emit!(MatchSummary {
            fills,
            still_crossed: m.is_crossed()
        });
        Ok(())
    }
}

#[derive(Accounts)]
pub struct InitializeMarket<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(init, payer = authority, space = Market::SPACE, seeds = [MARKET_SEED, authority.key().as_ref()], bump)]
    pub market: AccountLoader<'info, Market>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PlaceOrder<'info> {
    #[account(mut)]
    pub market: AccountLoader<'info, Market>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct CancelOrder<'info> {
    #[account(mut)]
    pub market: AccountLoader<'info, Market>,
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct MatchOrders<'info> {
    #[account(mut)]
    pub market: AccountLoader<'info, Market>,
}
