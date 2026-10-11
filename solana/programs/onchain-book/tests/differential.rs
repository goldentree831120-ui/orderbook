//! The on-chain program must trade exactly like the off-chain engine from M1-M4.
//!
//! Random flows of limit orders and cancels go through BOTH:
//!   - the Solana program, in LiteSVM, cranking match_orders after every place_order;
//!   - the `orderbook` crate (M1 engine), which matches inside submit().
//!
//! With a crank after every placement, the two designs must produce identical trades
//! (same maker, taker, price, qty, order) and identical resting books.
//! Differences that are by design (fixed capacity, owner checks) are handled explicitly.
mod common;

use common::*;
use onchain_book::{SIDE_BUY, SIDE_SELL};
use orderbook::{Event, OrderBook, OrderRequest, Side};
use solana_keypair::Keypair;

struct Rng(u64);
impl Rng {
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

type T = (u64, u64, u8, u64, u64); // maker, taker, taker side, price, qty

fn run(seed: u64, steps: usize) -> (usize, usize) {
    let mut env = setup();
    let users: Vec<Keypair> = (0..4).map(|_| env.user()).collect();
    let cranker = env.user();
    let mut off = OrderBook::new();
    let mut rng = Rng(seed);
    let mut owner_of: Vec<usize> = vec![usize::MAX]; // index by order id (ids start at 1)
    let (mut on_trades, mut off_trades): (Vec<T>, Vec<T>) = (vec![], vec![]);
    let mut cancels_checked = 0;

    for _ in 0..steps {
        if rng.next() % 10 < 7 || owner_of.len() == 1 {
            let u = (rng.next() % users.len() as u64) as usize;
            let side = if rng.next().is_multiple_of(2) {
                SIDE_BUY
            } else {
                SIDE_SELL
            };
            let price = rng.range(95, 105);
            let qty = rng.range(1, 20);
            match env.place(&users[u], side, price, qty) {
                Ok(_) => {}
                Err(e) if error_name(&e) == "BookFull" => continue, // by design: not sent off-chain either
                Err(e) => panic!("seed {seed}: unexpected place error {e}"),
            }
            let id = owner_of.len() as u64;
            owner_of.push(u);
            let s = if side == SIDE_BUY {
                Side::Buy
            } else {
                Side::Sell
            };
            for ev in off.submit(OrderRequest::limit(id, s, price, qty)) {
                if let Event::Trade(t) = ev {
                    off_trades.push((
                        t.maker_id,
                        t.taker_id,
                        if t.taker_side == Side::Buy {
                            SIDE_BUY
                        } else {
                            SIDE_SELL
                        },
                        t.price,
                        t.qty,
                    ));
                }
            }
            // crank until the book no longer crosses
            loop {
                let sent = env
                    .match_orders(&cranker, onchain_book::MAX_FILLS_PER_CALL)
                    .unwrap();
                on_trades.extend(
                    trades(&sent)
                        .iter()
                        .map(|t| (t.maker_id, t.taker_id, t.taker_side, t.price, t.qty)),
                );
                if !env.market().is_crossed() {
                    break;
                }
            }
        } else {
            let id = rng.range(1, owner_of.len() as u64 - 1);
            let owner = &users[owner_of[id as usize]];
            let on = env.cancel(owner, id);
            let off_ev = off.cancel(id);
            match (&on, off_ev) {
                (Ok(_), Event::Cancelled { .. }) => {}
                (Err(e), Event::Rejected { .. }) if error_name(e) == "OrderNotFound" => {}
                (on, off) => {
                    panic!("seed {seed}: cancel {id} disagrees: on-chain {on:?}, off-chain {off:?}")
                }
            }
            cancels_checked += 1;
        }
        assert_eq!(on_trades, off_trades, "seed {seed}: trades diverged");
    }

    // Final books must be identical, order by order.
    let m = env.market();
    for (side, onchain) in [(Side::Buy, m.bids()), (Side::Sell, m.asks())] {
        let offchain: Vec<(u64, u64, u64)> = off
            .depth(side, usize::MAX)
            .iter()
            .flat_map(|(p, _)| {
                off.level(side, *p)
                    .unwrap()
                    .orders()
                    .map(|o| (o.id, o.price, o.qty))
                    .collect::<Vec<_>>()
            })
            .collect();
        let onchain: Vec<(u64, u64, u64)> =
            onchain.iter().map(|o| (o.id, o.price, o.qty)).collect();
        assert_eq!(onchain, offchain, "seed {seed}: {side:?} book differs");
    }
    (on_trades.len(), cancels_checked)
}

#[test]
fn onchain_program_trades_exactly_like_the_rust_engine() {
    let mut total = (0, 0);
    for seed in 1..=8 {
        let (t, c) = run(seed, 250);
        total.0 += t;
        total.1 += c;
    }
    println!(
        "compared {} trades and {} cancels across 8 seeds x 250 steps",
        total.0, total.1
    );
    assert!(
        total.0 > 500,
        "flows should actually trade (got {})",
        total.0
    );
}
