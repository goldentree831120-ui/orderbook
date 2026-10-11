//! M4: the same input log always produces the same trades.

use std::process::Command as Proc;

use orderbook::replay::{format_command, generate, parse_log, replay, replay_into, Command};
use orderbook::{OrderBook, OrderRequest, Side};

const SAMPLE: &str = include_str!("../examples/sample.log");
const SAMPLE_EVENTS: &str = include_str!("data/sample.events");

/// The hand-checked sample log still produces exactly the committed event log.
/// If an engine change alters any event, this fails and the diff shows where.
#[test]
fn golden_sample_log() {
    let out = replay(&parse_log(SAMPLE).unwrap());
    assert_eq!(out.event_log(), SAMPLE_EVENTS);
    assert_eq!(out.trades, 4);
    assert_eq!(out.traded_qty, 13);
}

#[test]
fn same_log_twice_same_everything() {
    let cmds = generate(20_000, 7);
    let a = replay(&cmds);
    let b = replay(&cmds);
    assert_eq!(a.event_log(), b.event_log());
    assert_eq!(a.events_digest(), b.events_digest());
    assert_eq!(a.book_digest(), b.book_digest());
    assert!(
        a.trades > 1_000,
        "generator should produce real matching, got {} trades",
        a.trades
    );
}

/// `HashMap` uses a random seed per PROCESS, so replaying twice in one process can't show
/// that hash order doesn't leak into results. Run the real binary twice and compare.
#[test]
fn separate_processes_agree() {
    let dir = std::env::temp_dir().join(format!("ob-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("big.log");
    let text: String = generate(20_000, 99)
        .iter()
        .map(|c| format_command(c) + "\n")
        .collect();
    std::fs::write(&log, text).unwrap();

    let run = || {
        let out = Proc::new(env!("CARGO_BIN_EXE_replay"))
            .arg(&log)
            .output()
            .unwrap();
        assert!(out.status.success());
        // drop the timing line, which legitimately varies
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with("replay time"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (a, b) = (run(), run());
    assert_eq!(a, b);
    assert!(a.contains("events digest"));
}

/// Replaying a log in two halves (snapshot-and-continue) equals replaying it in one go.
#[test]
fn split_replay_equals_full_replay() {
    let cmds = generate(5_000, 3);
    let full = replay(&cmds);
    for split in [0, 1, 17, 2_500, 4_999, 5_000] {
        let first = replay(&cmds[..split]);
        let first_events = first.events.clone();
        let second = replay_into(first.book, &cmds[split..], split);
        let mut joined = first_events;
        joined.extend(second.events.iter().copied());
        assert_eq!(joined, full.events, "split at {split}");
        assert_eq!(second.book_digest(), full.book_digest(), "split at {split}");
    }
}

#[test]
fn generator_is_deterministic_and_seed_sensitive() {
    assert_eq!(generate(1_000, 5), generate(1_000, 5));
    assert_ne!(generate(1_000, 5), generate(1_000, 6));
}

#[test]
fn log_format_round_trips() {
    let cmds = generate(3_000, 11);
    let text: String = cmds.iter().map(|c| format_command(c) + "\n").collect();
    assert_eq!(parse_log(&text).unwrap(), cmds);
}

#[test]
fn replay_matches_driving_the_engine_directly() {
    let cmds = generate(2_000, 21);
    let mut book = OrderBook::new();
    let mut direct = Vec::new();
    for c in &cmds {
        match c {
            Command::Submit(r) => direct.extend(book.submit(*r)),
            Command::Cancel(id) => direct.push(book.cancel(*id)),
        }
    }
    let via_replay: Vec<_> = replay(&cmds).events.into_iter().map(|(_, e)| e).collect();
    assert_eq!(via_replay, direct);
}

#[test]
fn parse_errors_name_the_line() {
    let bad = "limit 1 buy 100 5\n\n# fine\nlimit 2 bye 100 5\n";
    let e = parse_log(bad).unwrap_err();
    assert_eq!(e.line, 4);
    assert!(e.message.contains("side"), "{e}");

    for (text, needle) in [
        ("frobnicate 1", "unknown command"),
        ("limit 1 buy 100", "wrong number of fields"),
        ("limit 1 buy -5 3", "price"),
        ("cancel x", "id"),
    ] {
        let e = parse_log(text).unwrap_err();
        assert_eq!(e.line, 1);
        assert!(e.message.contains(needle), "{text:?} -> {e}");
    }
}

#[test]
fn comments_and_short_sides_are_accepted() {
    let cmds =
        parse_log("  # header\nlimit 1 b 100 5   # trailing comment\nmarket 2 S 5\ncancel 1\n")
            .unwrap();
    assert_eq!(
        cmds,
        vec![
            Command::Submit(OrderRequest::limit(1, Side::Buy, 100, 5)),
            Command::Submit(OrderRequest::market(2, Side::Sell, 5)),
            Command::Cancel(1),
        ]
    );
}

/// The CLI's --check reports the first differing event and exits 1.
#[test]
fn cli_check_detects_tampered_event_log() {
    let dir = std::env::temp_dir().join(format!("ob-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let tampered = dir.join("tampered.events");
    std::fs::write(
        &tampered,
        SAMPLE_EVENTS.replacen("qty=5\n", "qty=6\n", 1), /* changes event 2 */
    )
    .unwrap();
    let sample = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/sample.log");

    let ok = Proc::new(env!("CARGO_BIN_EXE_replay"))
        .args([
            sample,
            "--check",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/sample.events"),
        ])
        .output()
        .unwrap();
    assert_eq!(ok.status.code(), Some(0));

    let bad = Proc::new(env!("CARGO_BIN_EXE_replay"))
        .args([sample, "--check"])
        .arg(&tampered)
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stdout).contains("MISMATCH at event 2"));
}
