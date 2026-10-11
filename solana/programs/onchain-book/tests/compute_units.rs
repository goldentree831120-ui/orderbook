//! Measure compute units (CU) for the worst cases of each instruction.
//! CU counts are deterministic (same program + same inputs = same count on any machine),
//! so unlike wall-clock benchmarks these numbers can be reported from any run.
//!
//!   cargo test --test compute_units -- --nocapture
mod common;

use common::*;
use onchain_book::{CAPACITY, MAX_FILLS_PER_CALL, SIDE_BUY, SIDE_SELL};

/// Default per-instruction budget when a transaction doesn't request more.
const DEFAULT_IX_BUDGET: u64 = 200_000;
/// Default per-transaction log limit; logs beyond it are truncated.
const LOG_LIMIT_BYTES: usize = 10_000;

#[test]
fn compute_units_worst_cases() {
    let mut rows: Vec<(String, u64)> = Vec::new();

    // place: empty book vs full side with the new order going to the FRONT (shift 63 orders).
    let mut env = setup();
    let a = env.user();
    rows.push((
        "place_order, empty book".into(),
        env.place(&a, SIDE_BUY, 50, 1).unwrap().cu,
    ));
    for i in 1..CAPACITY as u64 - 1 {
        env.place(&a, SIDE_BUY, 50 - (i % 40), 1).unwrap();
    }
    rows.push((
        format!(
            "place_order, {} resting, inserted at the front",
            CAPACITY - 1
        ),
        env.place(&a, SIDE_BUY, 99, 1).unwrap().cu,
    ));

    // cancel: front order of a full side (shift 63) vs back order (no shift).
    let front_id = env.market().bids()[0].id;
    let back_id = env.market().bids()[CAPACITY - 1].id;
    rows.push((
        format!("cancel_order, back of {CAPACITY}"),
        env.cancel(&a, back_id).unwrap().cu,
    ));
    rows.push((
        format!("cancel_order, front of {}", CAPACITY - 1),
        env.cancel(&a, front_id).unwrap().cu,
    ));

    // match: k single-lot fills, each removing a whole order from a full side.
    let mut per_k = Vec::new();
    let mut log_sizes = Vec::new();
    for k in [1u16, 8, MAX_FILLS_PER_CALL] {
        let mut env = setup();
        let a = env.user();
        for _ in 0..CAPACITY - 1 {
            env.place(&a, SIDE_SELL, 100, 1).unwrap();
        }
        env.place(&a, SIDE_BUY, 100, k as u64).unwrap();
        let s = env.match_orders(&a, k).unwrap();
        assert_eq!(trades(&s).len(), k as usize);
        let log_bytes: usize = s.logs.iter().map(|l| l.len()).sum();
        println!("match {k} fills: {log_bytes} bytes of logs");
        log_sizes.push((k, log_bytes));
        rows.push((
            format!("match_orders, {k} fills (each pops a full-side front)"),
            s.cu,
        ));
        per_k.push((k, s.cu));
    }
    let (k1, c1) = per_k[0];
    let (kn, cn) = per_k[per_k.len() - 1];
    let per_fill = (cn - c1) / (kn - k1) as u64;
    let max_fills_in_budget = (DEFAULT_IX_BUDGET - c1) / per_fill + 1;

    println!("\n| Instruction | Compute units |\n|---|---|");
    for (name, cu) in &rows {
        println!("| {name} | {cu} |");
        assert!(*cu < DEFAULT_IX_BUDGET, "{name} used {cu} CU");
    }
    let (lk1, l1) = log_sizes[0];
    let (lkn, ln) = log_sizes[log_sizes.len() - 1];
    let log_per_fill = (ln - l1) / (lkn - lk1) as usize;
    // Solana truncates a transaction's logs after 10,000 bytes; events past that are lost to clients.
    let max_fills_in_logs = (LOG_LIMIT_BYTES - l1) / log_per_fill + 1;
    println!("log bytes per fill ~ {log_per_fill}; fills before the {LOG_LIMIT_BYTES}-byte log limit ~ {max_fills_in_logs}");
    assert!((MAX_FILLS_PER_CALL as usize) < max_fills_in_logs);
    println!("\nmarginal cost per fill ~ {per_fill} CU; fills that fit in {DEFAULT_IX_BUDGET} CU ~ {max_fills_in_budget}");
    assert!(
        (MAX_FILLS_PER_CALL as u64) < max_fills_in_budget,
        "MAX_FILLS_PER_CALL={MAX_FILLS_PER_CALL} must stay under the measured limit {max_fills_in_budget}"
    );
}
