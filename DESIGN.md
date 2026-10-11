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
| Cancel (M1) | O(log P + k) | index O(1), find level O(log P), linear scan for the id in the level O(k), `VecDeque::remove` O(min(i, k−i)) |
| **Cancel (M3)** | **O(log P)** amortised | index O(1) gives the order's slot, find level O(log P), tombstone in O(1); compaction amortised O(1) |
| Best bid / ask | O(log P) | B-tree spine walk |
| Depth of n levels | O(log P + n) | |
| Lookup order by id | O(log P) | slot from the index (M3) |

Matching cost is proportional to the work actually done (fills), which is the
best you can do: every fill must emit a trade.

**The weak spot was cancel's O(k) scan.** In real markets most orders are
cancelled, not filled (cancel-to-trade ratios of 10:1 or higher are common),
and popular price levels can hold thousands of orders. M3 fixed it; see §7.

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

## 7. M3: benchmarks and the cancel optimisation

### What changed

Each resting order gets a **slot**: its absolute position in its level's queue
(`orders[0]` has slot `head`). The index stores `(side, price, slot)`, so cancel jumps
straight to `orders[slot - head]` and marks the order as a **tombstone** (qty = 0) instead
of removing it from the middle of the `VecDeque`.

- Tombstones are dropped as soon as they reach the front, so the front is always live and
  matching never skips anything.
- A level **compacts** (rebuilds its queue without tombstones and renumbers slots) when it
  has more than 32 dead entries *and* more dead than live. That keeps memory bounded and
  the cost amortised O(1) per cancel.

I chose this over the slab + intrusive linked list planned in M1. It keeps orders contiguous
in the `VecDeque` (cache-friendly matching), needs no `unsafe` and no second allocation per
order, and the change stays inside `Level`.

### Benchmarks (`benches/engine.rs`, criterion)

| Group | Varies | Expected shape |
|---|---|---|
| `insert_resting_limit/{10,1000,10000}` | price levels P | ~log P |
| `cancel_middle_of_level/{10,100,1000,10000}` | orders at one level k | O(k) before, ~flat after |
| `market_order_sweep/{1,10,100}` | levels swept L | linear in L |
| `mixed_flow/10k_ops` | realistic mix of limits, markets and cancels | regression check |

Each benchmark builds its book in untimed setup and returns it so the drop isn't timed.
(The first version timed the destructor of a 10,000-level book, which made insert look
O(P). The lesson: check that benchmark scaling matches the complexity you expect.)

### Reproduce before/after on your machine

```bash
git checkout 3a9e152            # M3 benchmarks, ORIGINAL engine
./scripts/bench.sh --save-baseline before
git checkout main               # optimised engine
./scripts/bench.sh --baseline before     # criterion prints the % change per benchmark
```

### Results

Measured on my laptop: **Intel Core i5-10210U @ 1.60 GHz, WSL2 Ubuntu, rustc 1.99.0**, criterion
defaults (100 samples), laptop on mains power. "Before" = commit `3a9e152` (M1 engine),
"after" = M3 engine. Before is derived from criterion's median after-time and median change.

| Benchmark | Before | After | Change (95% CI) |
|---|---|---|---|
| cancel_middle_of_level/100 | ~460 ns | 208 ns | −54.8% [−58.7, −50.8] |
| cancel_middle_of_level/1000 | ~3.67 µs | 452 ns | −87.7% [−89.4, −85.8] |
| cancel_middle_of_level/10000 | ~33.2 µs | 782 ns | −97.6% [−97.8, −97.3] |
| market_order_sweep/1 | ~1.23 µs | 1.17 µs | −4.7% [−7.0, −2.4] |
| market_order_sweep/10 | ~12.0 µs | 10.8 µs | −9.8% [−14.9, −4.8] |
| market_order_sweep/100 | ~96.6 µs | 102 µs | +5.4% [−0.8, +12.8], p = 0.15: no significant change |
| mixed_flow/10k_ops | ~3.09 ms | 3.35 ms | **+8.2% [+3.5, +13.4]: slower** |
| insert_resting_limit/*, cancel/10 | | | not captured in this run |

**Reading it honestly:**
- Cancel went from growing linearly with orders per level (~460 ns → ~33 µs as k goes
  100 → 10,000) to growing slowly (208 → 782 ns). At 10,000 orders it's ~42× faster. It
  still rises with k; the likely cause is cache misses in a bigger book, not scanning.
- The realistic mixed workload got **~8% slower**. Its levels are shallow, so there was no
  long scan to remove, and every operation now pays for slot bookkeeping and tombstone checks.
- So this is a trade: it is worth it if the venue has deep levels with heavy cancelling
  (typical of market makers), and not worth it for a thin book. A production engine would
  measure its real order flow before choosing.
- Sweeps improving 5–10% is within what I'd expect from noise plus layout changes; I don't
  claim it as an effect of the optimisation.

### Correctness of the optimisation

`tests/properties.rs` adds `heavy_cancels_on_one_level` (deep queue, many cancels, forces
compaction) on top of the M2 properties and the reference-model comparison.
`scripts/mutation-check.sh` plants 6 bugs, including 2 specific to slots ("compaction forgets
to renumber slots", "tombstone left at the front"). All 6 are caught; see
`docs/mutation-report.txt`. One planted bug (LIFO matching) makes the engine loop forever, so
the script treats a run over 120 s as caught.

## 8. M4: deterministic replay

**Why:** an exchange must be able to rebuild its book after a crash, settle disputes ("show me
why my order didn't fill"), and prove a new engine version behaves exactly like the old one.
All three need: same input log, same output, always.

**Log format** (`examples/sample.log`): one command per line, `limit <id> <side> <price> <qty>`,
`market <id> <side> <qty>`, `cancel <id>`, with `#` comments. It's plain text, so a log can be
read, diffed and hand-written. Bad input stops at the first error and names the line.

**Output:** a canonical event log (one event per line, prefixed by the index of the command
that caused it), a 64-bit FNV-1a digest of the events and one of the final book. The digests are
for quick comparison, not security.

**Why it is deterministic:**
- "time" is the engine's sequence number, not a clock;
- integers only, no floating point;
- no output depends on `HashMap` iteration order. The order index is a `HashMap`, and Rust
  seeds its hasher randomly *per process*, so a bug that leaked hash order into the output
  would make two runs of the same binary disagree.

**Tests** (`tests/replay.rs`):

| Test | What it proves |
|---|---|
| `golden_sample_log` | The hand-checked sample produces exactly `tests/data/sample.events`; any behaviour change shows up as a diff |
| `same_log_twice_same_everything` | 20,000 random commands, replayed twice: identical events and digests |
| `separate_processes_agree` | The real binary run twice as separate processes gives identical output: the random hash seed doesn't leak |
| `split_replay_equals_full_replay` | Replaying in two halves (snapshot, then continue) equals replaying in one go, at 6 split points |
| `replay_matches_driving_the_engine_directly` | The replay layer adds nothing and drops nothing |
| `log_format_round_trips`, `parse_errors_name_the_line`, `comments_and_short_sides_are_accepted` | The format |
| `cli_check_detects_tampered_event_log` | `--check` finds the first changed event and exits 1 |

Not covered (honest list): logs aren't versioned or checksummed per line; there is no
snapshot file format (split replay keeps the book in memory); ids must be unique for the whole
log, as in the engine.
