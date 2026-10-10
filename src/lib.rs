//! A price-time priority limit order book.
//!
//! ```
//! use orderbook::{OrderBook, OrderRequest, Side, Event};
//!
//! let mut book = OrderBook::new();
//! book.submit(OrderRequest::limit(1, Side::Sell, 101, 10));
//! let events = book.submit(OrderRequest::limit(2, Side::Buy, 101, 4));
//!
//! assert!(events.iter().any(|e| matches!(e, Event::Trade(t) if t.qty == 4 && t.price == 101)));
//! assert_eq!(book.depth(Side::Sell, 1), vec![(101, 6)]);
//! ```

pub mod book;
pub mod types;

pub use book::{Level, OrderBook};
pub use types::*;
