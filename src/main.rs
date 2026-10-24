//! `soroban-cost` — merge and compare Soroban cost snapshots.

#![forbid(unsafe_code)]

use soroban_cost_snapshot::diff::{self, Metric, Options};
use soroban_cost_snapshot::{collect, snapshot_dir, Snapshot};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
soroban-cost — per-function resource-cost snapshots for Soroban contracts

USAGE:
  soroban-cost collect [--dir DIR] --out FILE
      Merge the entries recorded by `cost_snapshot!` (in DIR, default
      $SOROBAN_COST_SNAPSHOT_DIR or target/soroban-cost-snapshot) into FILE.

  soroban-cost diff BASE HEAD [options]
      Compare two snapshot files. Exit code 1 if a selected metric rose by
      more than the threshold.

DIFF OPTIONS:
  --threshold PCT     allowed increase in percent (default 5)
  --metrics LIST      comma-separated: instructions, mem_bytes, read_entries,
                      write_entries, read_bytes, write_bytes, events_bytes,
                      fee_stroops, or `all` (default instructions,fee_stroops)
  --format FMT        markdown | text (default text)
  --fail-on-removed   also fail (exit 1) when an entry of BASE is missing
                      from HEAD (deleted, renamed or no longer run)
  --all               also list unchanged entries
  --max-rows N        maximum table rows in markdown (default 100)

A HEAD snapshot without entries is an error when BASE has entries.

EXIT CODES: 0 ok · 1 regression (or missing entry with --fail-on-removed)
            2 usage or input error
";

enum Fail {
    Usage(String),
    Input(String),
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(Fail::Usage(m)) => {
            eprintln!("error: {}\n\n{USAGE}", one_line(&m));
            ExitCode::from(2)
        }
        Err(Fail::Input(m)) => {
            eprintln!("error: {}", one_line(&m));
            ExitCode::from(2)
        }
    }
}

/// Escape control characters so an error message (which may quote text
/// from an untrusted file) stays on one line and cannot start a new line
/// with a CI workflow command such as `::error::`.
fn one_line(m: &str) -> String {
    m.chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn take_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, Fail> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| Fail::Usage(format!("{flag} needs a value")))
}

fn run(args: &[String]) -> Result<u8, Fail> {
    match args.first().map(String::as_str) {
        None | Some("-h") | Some("--help") | Some("help") => {
            print!("{USAGE}");
            Ok(0)
        }
        Some("-V") | Some("--version") => {
            println!("soroban-cost {}", env!("CARGO_PKG_VERSION"));
            Ok(0)
        }
        Some("collect") => cmd_collect(&args[1..]),
        Some("diff") => cmd_diff(&args[1..]),
        Some(other) => Err(Fail::Usage(format!("unknown command {other:?}"))),
    }
}

fn cmd_collect(args: &[String]) -> Result<u8, Fail> {
    let mut dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => dir = Some(take_value(args, &mut i, "--dir")?.into()),
            "--out" | "-o" => out = Some(take_value(args, &mut i, "--out")?.into()),
            other => return Err(Fail::Usage(format!("unexpected argument {other:?}"))),
        }
        i += 1;
    }
    let out = out.ok_or_else(|| Fail::Usage("collect needs --out FILE".into()))?;
    let dir = dir.unwrap_or_else(snapshot_dir);
    let snap = collect(&dir).map_err(|e| Fail::Input(e.to_string()))?;
    if snap.entries.is_empty() {
        eprintln!(
            "warning: no entries found in {} — did the tests call cost_snapshot!?",
            dir.display()
        );
    }
    snap.save(&out).map_err(|e| Fail::Input(e.to_string()))?;
    eprintln!("wrote {} entries to {}", snap.entries.len(), out.display());
    Ok(0)
}

fn cmd_diff(args: &[String]) -> Result<u8, Fail> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut opts = Options::default();
    let mut markdown = false;
    let mut show_all = false;
    let mut max_rows: usize = 100;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold" => {
                let v = take_value(args, &mut i, "--threshold")?;
                opts.threshold_pct = v
                    .trim()
                    .trim_end_matches('%')
                    .parse::<f64>()
                    .map_err(|_| Fail::Usage(format!("bad --threshold {v:?}")))?;
            }
            "--metrics" => {
                let v = take_value(args, &mut i, "--metrics")?;
                opts.metrics = Metric::parse_list(&v).map_err(Fail::Usage)?;
            }
            "--format" => match take_value(args, &mut i, "--format")?.as_str() {
                "markdown" | "md" => markdown = true,
                "text" => markdown = false,
                v => return Err(Fail::Usage(format!("bad --format {v:?}"))),
            },
            "--all" => show_all = true,
            "--fail-on-removed" => opts.fail_on_removed = true,
            "--max-rows" => {
                let v = take_value(args, &mut i, "--max-rows")?;
                max_rows = v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| Fail::Usage(format!("bad --max-rows {v:?}")))?;
            }
            s if s.starts_with('-') && s != "-" => {
                return Err(Fail::Usage(format!("unknown option {s:?}")))
            }
            s => files.push(s.into()),
        }
        i += 1;
    }
    if files.len() != 2 {
        return Err(Fail::Usage(
            "diff needs exactly two files: BASE HEAD".into(),
        ));
    }
    diff::validate(&opts).map_err(Fail::Usage)?;
    let base = Snapshot::load(&files[0]).map_err(|e| Fail::Input(e.to_string()))?;
    let head = Snapshot::load(&files[1]).map_err(|e| Fail::Input(e.to_string()))?;
    if head.entries.is_empty() && !base.entries.is_empty() {
        return Err(Fail::Input(format!(
            "{}: no entries (did the tests run and call cost_snapshot!?)",
            files[1].display()
        )));
    }
    let rep = diff::diff(&base, &head, &opts);
    if markdown {
        print!("{}", diff::render_markdown(&rep, &opts, show_all, max_rows));
    } else {
        print!("{}", diff::render_text(&rep, &opts, show_all));
    }
    Ok(if rep.failed(&opts) { 1 } else { 0 })
}
