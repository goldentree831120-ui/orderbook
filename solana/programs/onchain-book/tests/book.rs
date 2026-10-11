//! Functional tests: each rule of the on-chain book, through real transactions in LiteSVM.
mod common;

use common::*;
use onchain_book::{Market, CAPACITY, MAX_FILLS_PER_CALL, SIDE_BUY, SIDE_SELL};
use solana_signer::Signer;

#[test]
fn market_account_size_fits_the_cpi_limit() {
    let env = setup();
    assert_eq!(env.account_len(), Market::SPACE);
    // Checked at compile time: init via CPI allows at most 10 KiB.
    const _: () = assert!(Market::SPACE <= 10_240);
    let m = env.market();
    assert_eq!(m.authority, env.authority.pubkey());
    assert_eq!((m.bid_len, m.ask_len, m.next_order_id), (0, 0, 0));
}

#[test]
fn orders_rest_in_price_time_order() {
    let mut env = setup();
    let a = env.user();
    env.place(&a, SIDE_BUY, 99, 1).unwrap(); // id 1
    env.place(&a, SIDE_BUY, 101, 1).unwrap(); // id 2
    env.place(&a, SIDE_BUY, 100, 1).unwrap(); // id 3
    env.place(&a, SIDE_BUY, 101, 1).unwrap(); // id 4: same price as 2, behind it
    env.place(&a, SIDE_SELL, 105, 1).unwrap(); // id 5
    env.place(&a, SIDE_SELL, 103, 1).unwrap(); // id 6
    let m = env.market();
    let bid_ids: Vec<u64> = m.bids().iter().map(|o| o.id).collect();
    let ask_ids: Vec<u64> = m.asks().iter().map(|o| o.id).collect();
    assert_eq!(bid_ids, vec![2, 4, 3, 1]); // 101 (older), 101, 100, 99
    assert_eq!(ask_ids, vec![6, 5]); // 103, 105
}

#[test]
fn crossing_orders_trade_at_maker_price_with_partial_fills() {
    let mut env = setup();
    let (maker, taker, cranker) = (env.user(), env.user(), env.user());
    env.place(&maker, SIDE_SELL, 101, 5).unwrap(); // id 1
    env.place(&maker, SIDE_SELL, 102, 3).unwrap(); // id 2
    env.place(&taker, SIDE_BUY, 102, 6).unwrap(); // id 3, crosses both
    assert!(
        env.market().bid_len == 1,
        "place_order does not match by itself"
    );

    let sent = env.match_orders(&cranker, 10).unwrap();
    let t = trades(&sent);
    assert_eq!(t.len(), 2);
    assert_eq!(
        (
            t[0].maker_id,
            t[0].taker_id,
            t[0].price,
            t[0].qty,
            t[0].taker_side
        ),
        (1, 3, 101, 5, SIDE_BUY)
    );
    assert_eq!((t[1].maker_id, t[1].price, t[1].qty), (2, 102, 1)); // maker price, partial
    let m = env.market();
    assert_eq!(m.bid_len, 0);
    assert_eq!(m.asks()[0].id, 2);
    assert_eq!(m.asks()[0].qty, 2); // keeps its place with the remainder
    assert_eq!((m.trade_count, m.traded_qty), (2, 6));
}

#[test]
fn fifo_within_a_level() {
    let mut env = setup();
    let (a, b) = (env.user(), env.user());
    env.place(&a, SIDE_BUY, 100, 2).unwrap(); // id 1
    env.place(&b, SIDE_BUY, 100, 2).unwrap(); // id 2
    env.place(&a, SIDE_SELL, 100, 3).unwrap(); // id 3
    let t = trades(&env.match_orders(&b, 10).unwrap());
    assert_eq!(
        t.iter().map(|t| (t.maker_id, t.qty)).collect::<Vec<_>>(),
        vec![(1, 2), (2, 1)]
    );
}

#[test]
fn match_respects_max_fills_and_resumes() {
    let mut env = setup();
    let a = env.user();
    for _ in 0..5 {
        env.place(&a, SIDE_SELL, 100, 1).unwrap();
    }
    env.place(&a, SIDE_BUY, 100, 5).unwrap();
    assert_eq!(trades(&env.match_orders(&a, 2).unwrap()).len(), 2);
    assert!(env.market().is_crossed());
    assert_eq!(trades(&env.match_orders(&a, 10).unwrap()).len(), 3);
    assert!(!env.market().is_crossed());
    assert_eq!(trades(&env.match_orders(&a, 10).unwrap()).len(), 0); // nothing left: no-op, not an error
}

#[test]
fn max_fills_is_bounded() {
    let mut env = setup();
    let a = env.user();
    assert_eq!(
        error_name(&env.match_orders(&a, 0).unwrap_err()),
        "InvalidMaxFills"
    );
    assert_eq!(
        error_name(&env.match_orders(&a, MAX_FILLS_PER_CALL + 1).unwrap_err()),
        "InvalidMaxFills"
    );
    env.match_orders(&a, MAX_FILLS_PER_CALL).unwrap();
}

#[test]
fn cancel_by_owner_only() {
    let mut env = setup();
    let (owner, attacker) = (env.user(), env.user());
    env.place(&owner, SIDE_SELL, 101, 7).unwrap(); // id 1
    env.place(&owner, SIDE_SELL, 102, 1).unwrap(); // id 2
    assert_eq!(
        error_name(&env.cancel(&attacker, 1).unwrap_err()),
        "NotOwner"
    );
    assert_eq!(env.market().ask_len, 2);
    env.cancel(&owner, 1).unwrap();
    let m = env.market();
    assert_eq!(m.asks().iter().map(|o| o.id).collect::<Vec<_>>(), vec![2]);
    assert_eq!(
        error_name(&env.cancel(&owner, 1).unwrap_err()),
        "OrderNotFound"
    ); // already gone
    assert_eq!(
        error_name(&env.cancel(&owner, 999).unwrap_err()),
        "OrderNotFound"
    );
}

#[test]
fn invalid_orders_are_rejected_and_consume_no_id() {
    let mut env = setup();
    let a = env.user();
    assert_eq!(
        error_name(&env.place(&a, SIDE_BUY, 0, 1).unwrap_err()),
        "ZeroPrice"
    );
    assert_eq!(
        error_name(&env.place(&a, SIDE_BUY, 1, 0).unwrap_err()),
        "ZeroQuantity"
    );
    assert_eq!(
        error_name(&env.place(&a, 7, 1, 1).unwrap_err()),
        "InvalidSide"
    );
    assert_eq!(env.market().next_order_id, 0); // failed transactions change nothing
}

#[test]
fn full_side_is_rejected_other_side_still_works() {
    let mut env = setup();
    let a = env.user();
    for i in 0..CAPACITY as u64 {
        env.place(&a, SIDE_BUY, 100 - (i % 50), 1).unwrap();
    }
    assert_eq!(
        error_name(&env.place(&a, SIDE_BUY, 100, 1).unwrap_err()),
        "BookFull"
    );
    env.place(&a, SIDE_SELL, 200, 1).unwrap();
    env.cancel(&a, 1).unwrap();
    env.place(&a, SIDE_BUY, 100, 1).unwrap(); // room again after a cancel
}

#[test]
fn only_the_market_owner_pays_for_creation_and_market_is_per_authority() {
    let env1 = setup();
    let env2 = setup();
    assert_ne!(env1.market, env2.market); // different authorities, different PDAs
}
