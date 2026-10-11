# M5: the order book as a Solana program (Anchor 1.1.2)

`programs/onchain-book` is the M1 order book rebuilt as an on-chain program: one `Market`
account, fixed-capacity bids and asks, and three instructions: `place_order`, `cancel_order`
and `match_orders`. A differential test proves it trades exactly like the Rust engine.

```bash
cd solana
anchor keys sync          # first time only: writes YOUR program id into lib.rs and Anchor.toml
anchor build              # builds target/deploy/onchain_book.so and the IDL
cargo test                # LiteSVM tests, no validator needed (~45 s, mostly the differential test)
cargo test --test compute_units -- --nocapture   # prints the compute-unit table below
```

Tested with Solana CLI 3.1.10 (Agave), Anchor CLI 1.1.2, `anchor-lang = "=1.1.2"`, LiteSVM 0.10.

## Design

```
Market (PDA: ["market", authority])                      8,280 bytes, zero-copy
├── authority, next_order_id, trade_count, traded_qty, bid_len, ask_len, bump
├── bids: [Order; 64]   sorted: highest price first, then lowest id (oldest) first
└── asks: [Order; 64]   sorted: lowest price first, then oldest first
Order { id, price, qty, owner }                           64 bytes
```

| Instruction | Who | What | Cost |
|---|---|---|---|
| `initialize_market` | authority (pays rent) | creates the PDA | one-off |
| `place_order(side, price, qty)` | anyone (signer = owner) | inserts in price-time position; **does not match** | O(N) shift |
| `cancel_order(order_id)` | the order's owner only | removes it | O(N) scan + shift |
| `match_orders(max_fills)` | **anyone** (permissionless crank) | fills best bid vs best ask while they cross, up to `max_fills` (≤ 32); trade price = older order's price | O(fills × N) |

Order ids come from an on-chain counter, so a lower id is always an older order: the id is
also the time-priority sequence number, just like `Seq` in the Rust engine.

Events (`OrderPlaced`, `OrderCancelled`, `TradeEvent`, `MatchSummary`) are emitted with
`emit!`, so clients and indexers can rebuild every trade from transaction logs.

## Account-size limits and what they forced

- **10 KiB per account created by a program.** `init` creates the account through a CPI to
  the System Program, which can allocate at most 10,240 bytes at a time. Two sides × 64 orders
  × 64 bytes = 8 KiB of orders, 8,280 bytes in total, fits. (There's a compile-time assert in
  `tests/book.rs`.) A bigger book would need the client to create the account first (up to
  10 MiB) or `realloc` in 10 KiB steps.
- **4 KiB stack, 32 KiB heap.** A normal `#[account]` is deserialised into memory. An 8 KiB
  struct would overflow the stack. `#[account(zero_copy)]` reads and writes the orders in place
  in the account data instead.
- **Rent.** The market account must hold a rent-exempt balance (~0.06 SOL for 8,280 bytes),
  paid once by the authority.
- **Fixed capacity.** When a side holds 64 orders, `place_order` fails with `BookFull`. The
  Rust engine has no limit.

## Compute-unit limits (measured)

`cargo test --test compute_units -- --nocapture`. Compute units are deterministic: same program
and same input give the same count on every machine.

| Instruction | Compute units |
|---|---|
| place_order, empty book | 1,658 |
| place_order, 63 resting, inserted at the front (shifts 63 orders) | 1,684 |
| cancel_order, back of 64 | 1,959 |
| cancel_order, front of 63 (shifts 62) | 1,586 |
| match_orders, 1 fill | 1,605 |
| match_orders, 8 fills | 4,499 |
| match_orders, 32 fills, each removing the front of a full side | 14,339 |

- Shifting 63 orders costs almost nothing extra: `copy_within` compiles to the `sol_memmove`
  syscall, which is cheap. Most of the cost is Anchor's account checks and the event log.
- Each fill costs ~410 CU, so ~480 fills would fit in the default 200,000 CU per instruction.
  **Compute is not the binding limit. Logs are:** each fill logs ~70 bytes and Solana truncates
  a transaction's logs after 10,000 bytes, so above ~138 fills clients would silently lose trade
  events. `MAX_FILLS_PER_CALL = 32` keeps a wide margin under both (the test asserts it).
- That is also why matching is a separate crank: an unbounded `place_order` that swept the
  whole book could exceed either limit and fail, and with it the order.

## What I simplified compared with the Rust engine

| Rust engine (M1–M4) | On-chain (M5) | Why |
|---|---|---|
| Unlimited orders | 64 per side, `BookFull` | account size (above) |
| Matches inside `submit` | Rests first; `match_orders` crank matches | bounded compute per instruction |
| Limit **and** market orders | Limit only | a market order needs IOC semantics within one instruction, which needs the matching inside place; left out to keep place bounded |
| `HashMap` index → O(1) cancel (M3) | Linear scan, O(N) | no hash maps in an account; at N = 64 the scan is a few hundred CU |
| Ids chosen by the client | Ids from an on-chain counter | clients can't collide or reuse ids |
| No ownership | Owner stored per order; only the owner can cancel | on-chain anyone can send any instruction |
| No money | **No token settlement**: no escrow, no balances, no transfers | out of scope for M5; a real DEX escrows the quote/base tokens on place and settles on fill |

**Proof that the matching rules survived the port:** `tests/differential.rs` runs 8 random
flows (250 steps each, ~1,000 trades and ~600 cancels in total) through both this program and
the `orderbook` crate, cranking after every placement, and asserts identical trades and
identical final books order by order. Changing the program to print trades at the taker's
price makes it fail immediately.

**Behavioural difference to know about:** between `place_order` and the next crank, a crossing
order sits on the book. In that window its owner could cancel it (a free option), and the
order in which cranks land is up to the leader. The differential test cranks after every
placement, so it doesn't exercise this window; a production design would match inside
`place_order` up to a fill limit and only crank the remainder.

## Tests

| File | What |
|---|---|
| `tests/book.rs` | account size, price-time ordering, maker price and partial fills, FIFO, max_fills and resuming, owner-only cancel, invalid input, full book |
| `tests/compute_units.rs` | worst-case CU and log size per instruction; asserts MAX_FILLS_PER_CALL fits |
| `tests/differential.rs` | same random flows through the program and the Rust engine; identical trades and books |

All run in LiteSVM, an in-process Solana runtime that executes the real compiled `.so`
(same BPF program you deploy). Deploying to localnet or devnet and a TypeScript client are M6.
