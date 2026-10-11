//! Replay a command log and print what happened.
//!
//!   cargo run --release --bin replay -- examples/sample.log
//!   cargo run --release --bin replay -- examples/sample.log --events out.events
//!   cargo run --release --bin replay -- --generate 100000 --seed 42 > big.log
//!   cargo run --release --bin replay -- big.log --check expected.events
//!
//! Exit code 0 on success, 1 if `--check` finds a difference, 2 on bad input.
use std::process::ExitCode;
use std::{env, fs, io::Write};

use orderbook::replay::{book_snapshot, format_command, generate, parse_log, replay};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("usage: replay <log> [--events FILE] [--check FILE] [--book]\n       replay --generate N [--seed S]");
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<ExitCode, String> {
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args.get(i + 1).cloned())
    };

    if let Some(n) = flag("--generate") {
        let n: usize = n
            .ok_or("--generate needs a number")?
            .parse()
            .map_err(|_| "--generate needs a number")?;
        let seed: u64 = flag("--seed")
            .flatten()
            .unwrap_or_else(|| "1".into())
            .parse()
            .map_err(|_| "--seed needs a number")?;
        let mut out = std::io::BufWriter::new(std::io::stdout().lock());
        writeln!(out, "# generated: n={n} seed={seed}").map_err(|e| e.to_string())?;
        for c in generate(n, seed) {
            writeln!(out, "{}", format_command(&c)).map_err(|e| e.to_string())?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    let path = args
        .first()
        .filter(|a| !a.starts_with("--"))
        .ok_or("missing log file")?;
    let text = fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let commands = parse_log(&text).map_err(|e| format!("{path}: {e}"))?;

    let t0 = std::time::Instant::now();
    let out = replay(&commands);
    let elapsed = t0.elapsed();
    let log = out.event_log();

    println!("commands      {}", commands.len());
    println!("events        {}", out.events.len());
    println!("trades        {} ({} lots)", out.trades, out.traded_qty);
    println!(
        "resting       {} orders, {} lots",
        out.book.len(),
        out.book.resting_qty()
    );
    println!(
        "best bid/ask  {:?} / {:?}",
        out.book.best_bid(),
        out.book.best_ask()
    );
    println!("events digest {:016x}", out.events_digest());
    println!("book digest   {:016x}", out.book_digest());
    println!("replay time   {:.3} ms", elapsed.as_secs_f64() * 1e3);

    if args.iter().any(|a| a == "--book") {
        print!("\n{}", book_snapshot(&out.book));
    }
    if let Some(p) = flag("--events").flatten() {
        fs::write(&p, &log).map_err(|e| format!("{p}: {e}"))?;
        println!("wrote {p}");
    }
    if let Some(p) = flag("--check").flatten() {
        let expected = fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
        if let Some((i, (a, b))) = expected
            .lines()
            .zip(log.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
        {
            println!(
                "MISMATCH at event {}:\n  expected: {a}\n  got:      {b}",
                i + 1
            );
            return Ok(ExitCode::from(1));
        }
        if expected.lines().count() != log.lines().count() {
            println!(
                "MISMATCH: expected {} events, got {}",
                expected.lines().count(),
                log.lines().count()
            );
            return Ok(ExitCode::from(1));
        }
        println!("check OK: identical to {p}");
    }
    Ok(ExitCode::SUCCESS)
}
