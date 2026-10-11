#!/usr/bin/env bash
# Plants one bug at a time in src/book.rs and checks the property tests catch it.
set -uo pipefail
cd "$(dirname "$0")/.."
cp src/book.rs /tmp/book.rs.orig
trap 'cp /tmp/book.rs.orig src/book.rs' EXIT
mutate() {
  cp /tmp/book.rs.orig src/book.rs
  perl -0pi -e "$2" src/book.rs
  if cmp -s src/book.rs /tmp/book.rs.orig; then echo "?? $1: did not apply"; return; fi
  cargo build --release --tests -q >/dev/null 2>&1   # compile outside the time limit
  # A planted bug can make the engine loop forever (e.g. LIFO matching never empties the
  # front). That's a failure too, so a run longer than 120 s counts as caught.
  timeout 120 cargo test --release --test properties >/tmp/mut.log 2>&1
  local rc=$?
  if [ $rc -eq 0 ]; then echo "SURVIVED  $1"
  elif [ $rc -eq 124 ]; then echo "caught    $1   (engine hung: timeout)"
  else echo "caught    $1   ($(grep -oE '^test [a-z_]+ \.\.\. FAILED' /tmp/mut.log | awk '{print $2}' | tr '\n' ' '))"; fi
}
mutate "LIFO instead of FIFO within a level"   's/let Some\(maker\) = level\.orders\.front_mut\(\)/let Some(maker) = level.orders.back_mut()/'
mutate "buy limit excludes its own price"      's/Side::Buy => price <= lim,/Side::Buy => price < lim,/'
mutate "trade prints at taker limit"           's/price, \/\/ trades print at the maker.s price/price: limit.unwrap_or(price),/'
mutate "cancel forgets the cached level total" 's/        level\.total_qty -= remaining;\n//'
mutate "compaction forgets to renumber slots"  's/                entry\.2 = level\.head \+ i as Slot;\n//'
mutate "cancel leaves tombstone at the front"  's/        level\.live -= 1;\n        level\.trim_front\(\);/        level.live -= 1;/'
