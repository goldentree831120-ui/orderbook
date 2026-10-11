//! M4: deterministic replay of an event log.
//!
//! A **command log** is the input: every limit, market and cancel instruction in
//! arrival order, one per line. Replaying the same log through a fresh `OrderBook`
//! must always produce the same events, the same trades and the same final book.
//! That is what lets a venue rebuild state after a crash, audit a dispute, or prove two
//! engine versions behave identically.
//!
//! Log format (plain text, no dependencies, diff-friendly):
//!
//! ```text
//! # comments and blank lines are ignored
//! limit  <id> <buy|sell> <price> <qty>
//! market <id> <buy|sell> <qty>
//! cancel <id>
//! ```
//!
//! Why the engine is deterministic, and what would break it:
//! - "time" is the engine's own sequence number, never the wall clock;
//! - prices and quantities are integers (no float rounding that differs by CPU);
//! - nothing in the output depends on `HashMap` iteration order. The index is a
//!   `HashMap` with a random seed per process, so `tests/replay.rs` replays the same log
//!   in two separate processes to prove the seed doesn't leak into the results.

use std::fmt::{self, Write as _};

use crate::{
    CancelReason, Event, OrderBook, OrderId, OrderKind, OrderRequest, Price, Qty, RejectReason,
    Side,
};

/// One line of the command log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Submit(OrderRequest),
    Cancel(OrderId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parse a whole log. Stops at the first bad line and says which one and why.
pub fn parse_log(text: &str) -> Result<Vec<Command>, ParseError> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        out.push(parse_line(line).map_err(|message| ParseError {
            line: i + 1,
            message,
        })?);
    }
    Ok(out)
}

fn parse_line(line: &str) -> Result<Command, String> {
    let f: Vec<&str> = line.split_whitespace().collect();
    let num = |s: &str, what: &str| {
        s.parse::<u64>()
            .map_err(|_| format!("{what} must be a non-negative integer, got {s:?}"))
    };
    let side = |s: &str| match s.to_ascii_lowercase().as_str() {
        "buy" | "b" => Ok(Side::Buy),
        "sell" | "s" => Ok(Side::Sell),
        _ => Err(format!("side must be buy or sell, got {s:?}")),
    };
    match f.as_slice() {
        ["limit", id, s, price, qty] => Ok(Command::Submit(OrderRequest::limit(
            num(id, "id")?,
            side(s)?,
            num(price, "price")?,
            num(qty, "qty")?,
        ))),
        ["market", id, s, qty] => Ok(Command::Submit(OrderRequest::market(
            num(id, "id")?,
            side(s)?,
            num(qty, "qty")?,
        ))),
        ["cancel", id] => Ok(Command::Cancel(num(id, "id")?)),
        [op, ..] if !["limit", "market", "cancel"].contains(op) => {
            Err(format!("unknown command {op:?}"))
        }
        _ => Err(format!("wrong number of fields in {line:?}")),
    }
}

/// The log line for a command (inverse of `parse_log`).
pub fn format_command(c: &Command) -> String {
    match c {
        Command::Submit(r) => match r.kind {
            OrderKind::Limit { price } => {
                format!("limit {} {} {} {}", r.id, side_str(r.side), price, r.qty)
            }
            OrderKind::Market => format!("market {} {} {}", r.id, side_str(r.side), r.qty),
        },
        Command::Cancel(id) => format!("cancel {id}"),
    }
}

/// Everything a replay produced.
#[derive(Debug, Clone)]
pub struct ReplayOutput {
    /// `(index of the command that caused it, event)`, in order.
    pub events: Vec<(usize, Event)>,
    pub trades: usize,
    pub traded_qty: Qty,
    pub book: OrderBook,
}

impl ReplayOutput {
    /// Canonical text of every event, one per line. Two replays are identical iff this is.
    pub fn event_log(&self) -> String {
        let mut s = String::new();
        for (cmd, e) in &self.events {
            let _ = writeln!(s, "{cmd} {}", format_event(e));
        }
        s
    }

    /// Fingerprint of the event stream (FNV-1a 64). For quick comparison, not security.
    pub fn events_digest(&self) -> u64 {
        fnv1a(self.event_log().as_bytes())
    }

    /// Fingerprint of the final book: every live order, best price first, FIFO within a level.
    pub fn book_digest(&self) -> u64 {
        fnv1a(book_snapshot(&self.book).as_bytes())
    }
}

/// Replay commands through a fresh book.
pub fn replay(commands: &[Command]) -> ReplayOutput {
    replay_into(OrderBook::new(), commands, 0)
}

/// Replay commands on top of an existing book (used to check that replaying a log in two
/// halves gives the same result as replaying it in one go). `offset` numbers the commands.
pub fn replay_into(mut book: OrderBook, commands: &[Command], offset: usize) -> ReplayOutput {
    let mut events = Vec::new();
    for (i, c) in commands.iter().enumerate() {
        let evs = match c {
            Command::Submit(r) => book.submit(*r),
            Command::Cancel(id) => vec![book.cancel(*id)],
        };
        events.extend(evs.into_iter().map(|e| (offset + i, e)));
    }
    let trades: Vec<_> = events
        .iter()
        .filter_map(|(_, e)| {
            if let Event::Trade(t) = e {
                Some(t.qty)
            } else {
                None
            }
        })
        .collect();
    ReplayOutput {
        trades: trades.len(),
        traded_qty: trades.iter().sum(),
        events,
        book,
    }
}

/// Canonical one-line form of an event.
pub fn format_event(e: &Event) -> String {
    match e {
        Event::Accepted {
            id,
            side,
            kind,
            qty,
            ts,
        } => {
            let k = match kind {
                OrderKind::Limit { price } => format!("limit@{price}"),
                OrderKind::Market => "market".into(),
            };
            format!("accepted id={id} {} {k} qty={qty} ts={ts}", side_str(*side))
        }
        Event::Trade(t) => format!(
            "trade maker={} taker={} {} price={} qty={}",
            t.maker_id,
            t.taker_id,
            side_str(t.taker_side),
            t.price,
            t.qty
        ),
        Event::Rested {
            id,
            side,
            price,
            qty,
        } => format!("rested id={id} {} price={price} qty={qty}", side_str(*side)),
        Event::Cancelled {
            id,
            remaining,
            reason,
        } => {
            let r = match reason {
                CancelReason::UserRequested => "user",
                CancelReason::NoLiquidity => "no-liquidity",
            };
            format!("cancelled id={id} remaining={remaining} reason={r}")
        }
        Event::Rejected { id, reason } => {
            let r = match reason {
                RejectReason::ZeroQuantity => "zero-qty",
                RejectReason::ZeroPrice => "zero-price",
                RejectReason::DuplicateOrderId => "duplicate-id",
                RejectReason::UnknownOrderId => "unknown-id",
            };
            format!("rejected id={id} reason={r}")
        }
    }
}

/// Every live order on both sides: asks best (lowest) first, then bids best (highest) first,
/// orders within a level oldest first. Deterministic by construction.
pub fn book_snapshot(book: &OrderBook) -> String {
    let mut s = String::new();
    for side in [Side::Sell, Side::Buy] {
        for (price, _) in book.depth(side, usize::MAX) {
            let level = book
                .level(side, price)
                .expect("depth lists existing levels");
            for o in level.orders() {
                let _ = writeln!(
                    s,
                    "{} {} id={} qty={} ts={}",
                    side_str(side),
                    price,
                    o.id,
                    o.qty,
                    o.ts
                );
            }
        }
    }
    s
}

/// Deterministic pseudo-random command log for tests and demos (no `rand` dependency).
/// Same `(n, seed)` -> byte-identical log on every machine.
pub fn generate(n: usize, seed: u64) -> Vec<Command> {
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        // splitmix64
        x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut out = Vec::with_capacity(n);
    let mut next_id: OrderId = 1;
    for _ in 0..n {
        let r = next() % 100;
        let side = if next() % 2 == 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        if r < 60 {
            let price: Price = 95 + next() % 11;
            out.push(Command::Submit(OrderRequest::limit(
                next_id,
                side,
                price,
                1 + next() % 50,
            )));
            next_id += 1;
        } else if r < 75 {
            out.push(Command::Submit(OrderRequest::market(
                next_id,
                side,
                1 + next() % 100,
            )));
            next_id += 1;
        } else if next_id > 1 {
            out.push(Command::Cancel(1 + next() % (next_id - 1)));
        }
    }
    out
}

fn side_str(s: Side) -> &'static str {
    match s {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}
