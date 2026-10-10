//! `cargo run --example demo` — walk through a few orders and print events.

use orderbook::*;

fn show(book: &OrderBook) {
    println!(
        "    asks: {:?}",
        book.depth(Side::Sell, 5).iter().rev().collect::<Vec<_>>()
    );
    println!("    bids: {:?}", book.depth(Side::Buy, 5));
    println!();
}

fn main() {
    let mut book = OrderBook::new();
    let steps = [
        ("sell 5 @ 101", OrderRequest::limit(1, Side::Sell, 101, 5)),
        ("sell 3 @ 102", OrderRequest::limit(2, Side::Sell, 102, 3)),
        (
            "sell 2 @ 101 (queues behind #1)",
            OrderRequest::limit(3, Side::Sell, 101, 2),
        ),
        ("buy 4 @ 99", OrderRequest::limit(4, Side::Buy, 99, 4)),
        (
            "buy 9 @ 102 (sweeps 101, part of 102)",
            OrderRequest::limit(5, Side::Buy, 102, 9),
        ),
        (
            "market sell 10 (hits bids, rest cancelled)",
            OrderRequest::market(6, Side::Sell, 10),
        ),
    ];

    for (label, req) in steps {
        println!("> {label}");
        for e in book.submit(req) {
            println!("    {e:?}");
        }
        show(&book);
    }

    println!("> cancel #2");
    println!("    {:?}", book.cancel(2));
    show(&book);
}
