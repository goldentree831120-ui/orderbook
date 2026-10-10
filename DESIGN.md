# M1 — Core engine: design notes

This is the document to read until you can explain every line of `src/book.rs`
from memory. If an interviewer asks "why a BTreeMap?", the answer is in §2.

## 1. Rules the engine implements

| Rule | Where in code |
|---|---|
| **Price priority**: a buyer hits the *lowest* ask first; a seller hits the *highest* bid first. | `match_against_book`: `first_key_value` / `last_key_value` |
| **Time priority**: at the same price, the *oldest* order fills first. | `Level.orders` is a FIFO; we always fill `front_mut()` |
| **Trade price = maker's price.** A buy limit at 105 against a resting ask at 100 trades at 100 (the taker gets price improvement). | `Trade { price, .. }` uses the level price |
| **Limit order**: match while the opposite best price is within the limit, then rest the remainder. | `submit` step 4 |
| **Market order**: match at any price; any remainder is *cancelled*, never rested (immediate-or-cancel). | `CancelReason::NoLiquidity` |
| **Partial fills**: a maker that is partly filled keeps its place in the queue (its `ts` is not reset). | `maker.qty -= fill` in place |
| **Cancel**: removes a resting order; unknown/filled/already-cancelled ids are *rejected*, not ignored. | `cancel` |
| **Validation**: qty 0, limit price 0, and reused ids are rejected and consume no sequence number. | `validate` |

"Time" is an engine-assigned **sequence number** (`Seq`), not a wall clock.
Two orders can arrive in the same nanosecond; two sequence numbers can never be
equal. It also makes the engine fully deterministic, which M4 depends on.

Not implemented on purpose (scope for M1): self-trade prevention, order
amendment, stop orders, post-only, fees. Each would be a small, isolated
change at one of the steps in `submit`.

## 2. Data structures and why

```
bids:  BTreeMap<Price, Level>    keys ascending → best bid is the LAST key
asks:  BTreeMap<Price, Level>    keys ascending → best ask is the FIRST key
index: HashMap<OrderId, (Side, Price)>

Level { orders: VecDeque<Order>, total_qty: Qty }
```

### Price levels: `BTreeMap<Price, Level>`

The engine needs, constantly, three things from a side of the book:
the **best** price, **walking outward** from it in order, and **inserting /
removing a price** somewhere in the middle. A balanced ordered map gives all
three in `O(log P)` (P = number of distinct price levels on that side).

| Alternative | Why not (for now) |
|---|---|
| `HashMap<Price, Level>` | O(1) lookup but no ordering: finding the best price is O(P). |
| `BinaryHeap` of prices | O(1) peek at best, but can't remove an arbitrary level (cancel empties a mid-book level) and can't iterate in order for depth. |
| Sorted `Vec<(Price, Level)>` | Best price O(1), very cache-friendly, but inserting a new level is O(P) memmove. Often *faster* in practice because P is small and most activity is near the top. A real candidate for M3. |
| Array indexed by tick `[Level; MAX_TICKS]` | Everything O(1), but needs a bounded price range and wastes memory on empty ticks. This is what many HFT engines do, and it is the shape the Solana version (M5) will be forced into. |

Rust's `BTreeMap` is a B-tree (not a red-black tree): each node holds up to 11
keys in a contiguous array, so it touches far fewer cache lines than a binary
tree. Note `first_key_value()` is still O(log P) — it walks down the left
spine — not O(1). If best-price lookup ever dominates a profile, caching the
best price is an easy M3 optimisation.

Bids and asks use the same ascending map; the bid side just reads from the end
(`last_key_value`, `.iter().rev()`). An alternative is `BTreeMap<Reverse<Price>, Level>`
for bids so both sides read "best first"; I chose the plain one to keep a
single key type and keep the asymmetry visible in one `match`.

### Orders within a level: `VecDeque<Order>`

A price level is a queue: new orders join the back, fills come off the front.
`VecDeque` is a growable ring buffer, so `push_back` and `pop_front` are O(1)
(amortised) and the orders are contiguous in memory.

`Order` is `Copy` and 40 bytes; storing it inline (not `Box<Order>`) means
matching walks a contiguous buffer instead of chasing pointers.

`total_qty` is cached so depth queries don't sum the queue every time. It is
updated on every fill, rest and cancel, and `assert_invariants` checks it.

### Cancel lookup: `HashMap<OrderId, (Side, Price)>`

Without it, cancel would have to search every level: O(N). With it, we jump
straight to the right level in O(1) + O(log P). The index holds only *resting*
orders; an entry is removed when the order is fully filled or cancelled.

`seen_ids: HashSet<OrderId>` is separate and remembers every id ever accepted,
so ids can't be reused. It grows forever; a production engine would scope ids
per session or trading day.

## 3. Complexity

Notation: **P** = price levels on the relevant side, **k** = orders at one
level, **m** = number of fills an incoming order produces, **L** = number of
levels it fully consumes, **n** = levels requested.

| Operation | Cost | Breakdown |
|---|---|---|
| Limit order that rests without matching | **O(log P)** | best-price check O(log P) + `entry()` O(log P) + `push_back` O(1) + index insert O(1) |
| Limit/market order that matches | **O(m + (L+1)·log P)** | each fill O(1); each level visited costs one best-price lookup + `get_mut` O(log P); each emptied level `remove` O(log P) |
| Cancel | **O(log P + k)** | index O(1), find level O(log P), linear scan for the id in the level O(k), `VecDeque::remove` O(min(i, k−i)) |
| Best bid / ask | O(log P) | B-tree spine walk |
| Depth of n levels | O(log P + n) | |
| Lookup order by id | O(log P + k) | |

Matching cost is proportional to the work actually done (fills), which is the
best you can do: every fill must emit a trade.

**The weak spot is cancel's O(k) scan.** In real markets most orders are
cancelled, not filled (cancel-to-trade ratios of 10:1 or higher are common),
and popular price levels can hold thousands of orders. M3's optimisation will
target this: give each order a stable slot in a slab and link a level's orders
as an intrusive doubly-linked list, making cancel O(1) after the lookup.
Before changing it, M3 will benchmark it so the before/after is real.

## 4. Event model

Every command returns the events it caused, in order:

```
submit(buy 5 @ 100) with 3 resting @ 100
  Accepted { id: 2, qty: 5, ts: 1 }
  Trade    { maker_id: 1, taker_id: 2, price: 100, qty: 3 }
  Rested   { id: 2, price: 100, qty: 2 }
```

The engine performs no I/O, so it is trivial to test, benchmark and replay.
The event stream is enough to rebuild the book, and gives the conservation law
M2 will test as a property:

```
Σ Accepted.qty  =  2·Σ Trade.qty  +  resting qty  +  Σ Cancelled.remaining
```

(Each trade reduces *both* the maker and the taker by `qty`, hence the 2.)
`tests/engine.rs::random_stream_keeps_invariants_and_conserves_quantity`
already checks this over 5,000 random operations as a preview.

## 5. Invariants (`OrderBook::assert_invariants`)

1. Not crossed: best bid < best ask after every command.
2. No empty levels are left in the maps.
3. Each level's `total_qty` equals the sum of its orders.
4. Every resting order has qty > 0, sits on the correct side and price.
5. Within a level, `ts` is strictly increasing (FIFO).
6. The index and the book agree exactly (same set of ids, same locations).

## 6. Edge cases worth being able to explain

- **Integer prices.** `Price` and `Qty` are `u64` ticks/lots. Floats would make
  equality and ordering unreliable and break determinism.
- **Why the book can't end up crossed.** A limit order only rests *after*
  matching stops, and matching only stops when the opposite best price no
  longer crosses the limit (or the side is empty).
- **Borrowing.** `match_against_book` needs `&mut` to one side's map and to the
  index at the same time. Splitting the borrow by field
  (`(&mut self.asks, &mut self.index)`) satisfies the borrow checker without
  `RefCell` or cloning.
- **Rejected orders don't consume a `Seq`,** so a replayed log with the same
  valid orders always yields the same timestamps.
