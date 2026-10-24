use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_soroban-cost"))
}

fn run(args: &[&str]) -> Output {
    bin().args(args).output().expect("binary runs")
}

fn write_snap(path: &Path, items: &[(&str, u64, u64)]) {
    let mut entries = serde_json::Map::new();
    for (n, i, f) in items {
        entries.insert(
            n.to_string(),
            serde_json::json!({
                "instructions": i, "mem_bytes": 0, "read_entries": 0, "write_entries": 0,
                "read_bytes": 0, "write_bytes": 0, "events_bytes": 0, "fee_stroops": f
            }),
        );
    }
    let v = serde_json::json!({ "version": 1, "entries": entries });
    fs::write(path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

#[test]
fn help_and_version() {
    let o = run(&["--help"]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("USAGE"));
    let o = run(&["--version"]);
    assert!(o.status.success());
}

#[test]
fn unknown_command_is_exit_2() {
    assert_eq!(run(&["frobnicate"]).status.code(), Some(2));
    assert_eq!(run(&["diff", "only-one"]).status.code(), Some(2));
    assert_eq!(
        run(&["diff", "a", "b", "--threshold"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&["diff", "a", "b", "--threshold", "abc"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&["diff", "a", "b", "--threshold", "NaN"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&["diff", "a", "b", "--threshold", "-3"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&["diff", "a", "b", "--metrics", "gas"]).status.code(),
        Some(2)
    );
    assert_eq!(
        run(&["diff", "a", "b", "--max-rows", "0"]).status.code(),
        Some(2)
    );
    assert_eq!(run(&["collect"]).status.code(), Some(2));
}

#[test]
fn diff_exit_codes_and_markdown() {
    let d = tempfile::tempdir().unwrap();
    let base = d.path().join("base.json");
    let head = d.path().join("head.json");
    write_snap(&base, &[("c::incr", 1000, 50), ("c::get", 500, 20)]);
    write_snap(&head, &[("c::incr", 1000, 50), ("c::get", 500, 20)]);
    let o = run(&["diff", base.to_str().unwrap(), head.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0));

    write_snap(&head, &[("c::incr", 1200, 55), ("c::get", 500, 20)]);
    let o = run(&[
        "diff",
        base.to_str().unwrap(),
        head.to_str().unwrap(),
        "--format",
        "markdown",
    ]);
    assert_eq!(o.status.code(), Some(1));
    let md = String::from_utf8_lossy(&o.stdout);
    assert!(
        md.contains("| `c::incr` | 1,000 → 1,200 (+20.0%) | 50 → 55 (+10.0%) | 🔴 regression |"),
        "{md}"
    );

    // A higher threshold lets it pass.
    let o = run(&[
        "diff",
        base.to_str().unwrap(),
        head.to_str().unwrap(),
        "--threshold",
        "25%",
    ]);
    assert_eq!(o.status.code(), Some(0));
}

#[test]
fn diff_rejects_bad_input_files() {
    let d = tempfile::tempdir().unwrap();
    let good = d.path().join("good.json");
    write_snap(&good, &[("a", 1, 1)]);
    let bad = d.path().join("bad.json");
    for content in [
        "",
        "{",
        "[]",
        r#"{"version":1}"#,
        r#"{"version":9,"entries":{}}"#,
        r#"{"version":1,"entries":{"a b":{"instructions":1,"mem_bytes":0,"read_entries":0,"write_entries":0,"read_bytes":0,"write_bytes":0,"events_bytes":0,"fee_stroops":0}}}"#,
    ] {
        fs::write(&bad, content).unwrap();
        let o = run(&["diff", good.to_str().unwrap(), bad.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(2), "content {content:?}");
    }
    let missing = d.path().join("missing.json");
    let o = run(&["diff", good.to_str().unwrap(), missing.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn collect_then_diff() {
    let d = tempfile::tempdir().unwrap();
    let rec = d.path().join("rec");
    soroban_cost_snapshot::record_in(
        &rec,
        "c::incr",
        soroban_cost_snapshot::Entry {
            instructions: 10,
            ..Default::default()
        },
    )
    .unwrap();
    let out = d.path().join("snap.json");
    let o = run(&[
        "collect",
        "--dir",
        rec.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = run(&[
        "diff",
        out.to_str().unwrap(),
        out.to_str().unwrap(),
        "--all",
    ]);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).contains("unchanged"));
}

#[test]
fn collect_missing_dir_is_exit_2() {
    let d = tempfile::tempdir().unwrap();
    let o = run(&[
        "collect",
        "--dir",
        d.path().join("nope").to_str().unwrap(),
        "--out",
        d.path().join("o.json").to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn empty_head_is_error_and_removed_can_fail() {
    let d = tempfile::tempdir().unwrap();
    let base = d.path().join("base.json");
    let head = d.path().join("head.json");
    write_snap(&base, &[("a", 1, 1), ("b", 1, 1)]);
    write_snap(&head, &[]);
    assert_eq!(
        run(&["diff", base.to_str().unwrap(), head.to_str().unwrap()])
            .status
            .code(),
        Some(2)
    );
    write_snap(&head, &[("a", 1, 1)]);
    assert_eq!(
        run(&["diff", base.to_str().unwrap(), head.to_str().unwrap()])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        run(&[
            "diff",
            base.to_str().unwrap(),
            head.to_str().unwrap(),
            "--fail-on-removed"
        ])
        .status
        .code(),
        Some(1)
    );
}

#[test]
fn error_messages_cannot_inject_workflow_commands() {
    let d = tempfile::tempdir().unwrap();
    let good = d.path().join("good.json");
    write_snap(&good, &[("a", 1, 1)]);
    let bad = d.path().join("bad.json");
    fs::write(
        &bad,
        "{\"version\":1,\"entries\":{},\"\\n::error title=x::forged\":1}",
    )
    .unwrap();
    let o = run(&["diff", good.to_str().unwrap(), bad.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!err.lines().any(|l| l.starts_with("::")), "{err}");
}
