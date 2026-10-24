//! Compare two snapshots and render the result.

use crate::{is_valid_name, Entry, Snapshot};
use std::fmt::Write as _;

/// A cost metric that can be compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    Instructions,
    MemBytes,
    ReadEntries,
    WriteEntries,
    ReadBytes,
    WriteBytes,
    EventsBytes,
    FeeStroops,
}

impl Metric {
    pub const ALL: [Metric; 8] = [
        Metric::Instructions,
        Metric::MemBytes,
        Metric::ReadEntries,
        Metric::WriteEntries,
        Metric::ReadBytes,
        Metric::WriteBytes,
        Metric::EventsBytes,
        Metric::FeeStroops,
    ];

    /// Key used on the command line and in JSON.
    pub fn key(self) -> &'static str {
        match self {
            Metric::Instructions => "instructions",
            Metric::MemBytes => "mem_bytes",
            Metric::ReadEntries => "read_entries",
            Metric::WriteEntries => "write_entries",
            Metric::ReadBytes => "read_bytes",
            Metric::WriteBytes => "write_bytes",
            Metric::EventsBytes => "events_bytes",
            Metric::FeeStroops => "fee_stroops",
        }
    }

    /// Human label for tables.
    pub fn label(self) -> &'static str {
        match self {
            Metric::Instructions => "CPU instructions",
            Metric::MemBytes => "Memory (bytes)",
            Metric::ReadEntries => "Read entries",
            Metric::WriteEntries => "Write entries",
            Metric::ReadBytes => "Read bytes",
            Metric::WriteBytes => "Write bytes",
            Metric::EventsBytes => "Event bytes",
            Metric::FeeStroops => "Est. fee (stroops)",
        }
    }

    pub fn get(self, e: &Entry) -> u64 {
        match self {
            Metric::Instructions => e.instructions,
            Metric::MemBytes => e.mem_bytes,
            Metric::ReadEntries => e.read_entries,
            Metric::WriteEntries => e.write_entries,
            Metric::ReadBytes => e.read_bytes,
            Metric::WriteBytes => e.write_bytes,
            Metric::EventsBytes => e.events_bytes,
            Metric::FeeStroops => e.fee_stroops,
        }
    }

    pub fn parse(s: &str) -> Option<Metric> {
        Metric::ALL.iter().copied().find(|m| m.key() == s.trim())
    }

    /// Parse a comma-separated list; `all` selects every metric.
    pub fn parse_list(s: &str) -> Result<Vec<Metric>, String> {
        if s.trim() == "all" {
            return Ok(Metric::ALL.to_vec());
        }
        let mut out = Vec::new();
        for part in s.split(',').filter(|p| !p.trim().is_empty()) {
            let m = Metric::parse(part).ok_or_else(|| {
                let keys: Vec<_> = Metric::ALL.iter().map(|m| m.key()).collect();
                format!(
                    "unknown metric {:?} (use: {} or all)",
                    part.trim(),
                    keys.join(", ")
                )
            })?;
            if !out.contains(&m) {
                out.push(m);
            }
        }
        if out.is_empty() {
            return Err("no metrics selected".into());
        }
        Ok(out)
    }
}

/// Diff options.
#[derive(Debug, Clone)]
pub struct Options {
    /// Metrics that decide pass/fail and are shown in the table.
    pub metrics: Vec<Metric>,
    /// An increase strictly greater than this percentage is a regression.
    pub threshold_pct: f64,
    /// Treat entries that exist in the base but not in the head as a failure
    /// (catches deleted or no-longer-running cost tests).
    pub fail_on_removed: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            metrics: vec![Metric::Instructions, Metric::FeeStroops],
            threshold_pct: 5.0,
            fail_on_removed: false,
        }
    }
}

/// Result for one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unchanged,
    /// Some selected metric went down and none went up.
    Improved,
    /// Something changed, but no selected metric rose above the threshold.
    Changed,
    /// A selected metric rose above the threshold.
    Regressed,
    Added,
    Removed,
}

#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    pub base: Option<Entry>,
    pub head: Option<Entry>,
    pub status: Status,
}

#[derive(Debug, Clone)]
pub struct Report {
    pub rows: Vec<Row>,
}

impl Report {
    pub fn count(&self, s: Status) -> usize {
        self.rows.iter().filter(|r| r.status == s).count()
    }
    pub fn has_regression(&self) -> bool {
        self.count(Status::Regressed) > 0
    }
    /// True if the check should fail: a regression, or (when
    /// `fail_on_removed`) an entry that disappeared from the head snapshot.
    pub fn failed(&self, opts: &Options) -> bool {
        self.has_regression() || (opts.fail_on_removed && self.count(Status::Removed) > 0)
    }
}

/// Percentage change from `base` to `head`.
/// `None` means "infinite" (base 0, head > 0).
pub fn pct_change(base: u64, head: u64) -> Option<f64> {
    if base == 0 {
        return if head == 0 { Some(0.0) } else { None };
    }
    Some((head as f64 - base as f64) / base as f64 * 100.0)
}

/// Largest accepted threshold (percent).
pub const MAX_THRESHOLD_PCT: f64 = 1_000_000.0;

/// Exact check: is `(head - base) / base * 100 > threshold`?
///
/// Uses integer math (threshold rounded to 0.001%) so there are no
/// floating-point surprises at the boundary or for values above 2^53.
fn is_regression(base: u64, head: u64, threshold: f64) -> bool {
    if head <= base {
        return false;
    }
    if base == 0 {
        return true;
    }
    // threshold in milli-percent; validate() bounds it, clamp defensively.
    let t_milli = (threshold.clamp(0.0, MAX_THRESHOLD_PCT) * 1000.0).round() as u128;
    let delta = (head - base) as u128;
    // delta / base * 100 > t_milli / 1000  <=>  delta * 100_000 > t_milli * base
    delta * 100_000 > t_milli * base as u128
}

/// Validate options.
pub fn validate(opts: &Options) -> Result<(), String> {
    if !opts.threshold_pct.is_finite()
        || opts.threshold_pct < 0.0
        || opts.threshold_pct > MAX_THRESHOLD_PCT
    {
        return Err(format!(
            "threshold must be a number between 0 and {}, got {}",
            MAX_THRESHOLD_PCT, opts.threshold_pct
        ));
    }
    if opts.metrics.is_empty() {
        return Err("no metrics selected".into());
    }
    Ok(())
}

/// Compare `base` against `head`.
pub fn diff(base: &Snapshot, head: &Snapshot, opts: &Options) -> Report {
    let mut names: Vec<&String> = base.entries.keys().chain(head.entries.keys()).collect();
    names.sort();
    names.dedup();
    let rows = names
        .into_iter()
        .map(|name| {
            let b = base.entries.get(name).copied();
            let h = head.entries.get(name).copied();
            let status = match (b, h) {
                (None, Some(_)) => Status::Added,
                (Some(_), None) => Status::Removed,
                (Some(b), Some(h)) => {
                    if b == h {
                        Status::Unchanged
                    } else if opts
                        .metrics
                        .iter()
                        .any(|m| is_regression(m.get(&b), m.get(&h), opts.threshold_pct))
                    {
                        Status::Regressed
                    } else if opts.metrics.iter().any(|m| m.get(&h) < m.get(&b))
                        && !opts.metrics.iter().any(|m| m.get(&h) > m.get(&b))
                    {
                        Status::Improved
                    } else {
                        Status::Changed
                    }
                }
                (None, None) => unreachable!("name comes from one of the maps"),
            };
            Row {
                name: name.clone(),
                base: b,
                head: h,
                status,
            }
        })
        .collect();
    Report { rows }
}

/// Format an integer with thin thousands separators (ASCII comma).
pub fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Percentage for display, rounded *away from zero* to 0.1 so that an
/// increase never displays as smaller than it is (e.g. +5.04% -> "+5.1%",
/// and a tiny increase -> "+0.1%" instead of "0%").
fn fmt_pct(base: u64, head: u64) -> String {
    if head == base {
        return "0%".into();
    }
    if base == 0 {
        return "new".into();
    }
    // Sign from the exact integer comparison; magnitude from the exact
    // integer difference, rounded up to 0.1 (never shown as 0).
    let p = head.abs_diff(base) as f64 / base as f64 * 100.0;
    let r = ((p * 10.0).ceil() / 10.0).max(0.1);
    let sign = if head > base { '+' } else { '-' };
    format!("{}{:.1}%", sign, r)
}

fn status_md(s: Status) -> &'static str {
    match s {
        Status::Unchanged => "unchanged",
        Status::Improved => "🟢 improved",
        Status::Changed => "changed",
        Status::Regressed => "🔴 regression",
        Status::Added => "🆕 added",
        Status::Removed => "removed",
    }
}

/// Defensive escape for table cells. Names are already restricted by
/// [`is_valid_name`]; this is a second line of defence.
fn md_name(name: &str) -> String {
    if is_valid_name(name) {
        format!("`{}`", name)
    } else {
        "`<invalid name>`".into()
    }
}

fn cell(m: Metric, r: &Row) -> String {
    match (r.base, r.head) {
        (Some(b), Some(h)) => {
            let (bv, hv) = (m.get(&b), m.get(&h));
            if bv == hv {
                fmt_num(hv)
            } else {
                format!("{} → {} ({})", fmt_num(bv), fmt_num(hv), fmt_pct(bv, hv))
            }
        }
        (None, Some(h)) => fmt_num(m.get(&h)),
        (Some(b), None) => format!("~~{}~~", fmt_num(m.get(&b))),
        (None, None) => String::new(),
    }
}

/// Render a Markdown report (for a GitHub job summary or PR comment).
///
/// By default only rows that changed are shown; `show_all` shows every row.
/// At most `max_rows` rows are printed.
pub fn render_markdown(rep: &Report, opts: &Options, show_all: bool, max_rows: usize) -> String {
    let mut s = String::new();
    let reg = rep.count(Status::Regressed);
    let removed = rep.count(Status::Removed);
    let _ = writeln!(s, "### Soroban cost snapshot");
    let _ = writeln!(s);
    if opts.fail_on_removed && removed > 0 {
        let _ = writeln!(
            s,
            "**{} entr{} missing** from this run (test deleted, renamed or not run?).\n",
            removed,
            if removed == 1 { "y" } else { "ies" }
        );
    }
    if reg > 0 {
        let _ = writeln!(
            s,
            "**{} regression(s)** above the {}% threshold.",
            reg,
            trim_float(opts.threshold_pct)
        );
    } else {
        let _ = writeln!(
            s,
            "No regressions above the {}% threshold.",
            trim_float(opts.threshold_pct)
        );
    }
    let _ = writeln!(
        s,
        "\n{} entries · {} regressed · {} improved · {} changed · {} added · {} removed · {} unchanged",
        rep.rows.len(),
        reg,
        rep.count(Status::Improved),
        rep.count(Status::Changed),
        rep.count(Status::Added),
        rep.count(Status::Removed),
        rep.count(Status::Unchanged)
    );
    let shown: Vec<&Row> = rep
        .rows
        .iter()
        .filter(|r| show_all || r.status != Status::Unchanged)
        .collect();
    if shown.is_empty() {
        let _ = writeln!(s, "\nNo cost changes.");
        return s;
    }
    // Regressions first, then the rest in name order.
    let mut shown = shown;
    shown.sort_by_key(|r| (r.status != Status::Regressed, r.name.clone()));
    let _ = writeln!(s);
    let _ = write!(s, "| Entry |");
    for m in &opts.metrics {
        let _ = write!(s, " {} |", m.label());
    }
    let _ = writeln!(s, " Status |");
    let _ = write!(s, "|---|");
    for _ in &opts.metrics {
        let _ = write!(s, "---:|");
    }
    let _ = writeln!(s, "---|");
    for r in shown.iter().take(max_rows) {
        let _ = write!(s, "| {} |", md_name(&r.name));
        for m in &opts.metrics {
            let _ = write!(s, " {} |", cell(*m, r));
        }
        let _ = writeln!(s, " {} |", status_md(r.status));
    }
    if shown.len() > max_rows {
        let _ = writeln!(s, "\n… and {} more rows not shown.", shown.len() - max_rows);
    }
    let _ = writeln!(
        s,
        "\n<sub>Costs measured in tests with soroban-sdk's cost estimate; native (non-Wasm) test contracts under-report. Fees are estimates.</sub>"
    );
    s
}

/// Plain-text report for terminals.
pub fn render_text(rep: &Report, opts: &Options, show_all: bool) -> String {
    let mut s = String::new();
    for r in rep
        .rows
        .iter()
        .filter(|r| show_all || r.status != Status::Unchanged)
    {
        let name = if is_valid_name(&r.name) {
            r.name.as_str()
        } else {
            "<invalid name>"
        };
        let _ = write!(
            s,
            "{:<10} {}",
            format!("{:?}", r.status).to_lowercase(),
            name
        );
        for m in &opts.metrics {
            let _ = write!(s, "  {}={}", m.key(), cell(*m, r).replace('~', ""));
        }
        let _ = writeln!(s);
    }
    let _ = writeln!(
        s,
        "{} regression(s) above {}%",
        rep.count(Status::Regressed),
        trim_float(opts.threshold_pct)
    );
    s
}

fn trim_float(f: f64) -> String {
    // `+ 0.0` turns -0.0 into 0.0.
    format!("{}", f + 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn snap(items: &[(&str, u64, u64)]) -> Snapshot {
        let mut entries = BTreeMap::new();
        for (n, i, f) in items {
            entries.insert(
                n.to_string(),
                Entry {
                    instructions: *i,
                    fee_stroops: *f,
                    ..Default::default()
                },
            );
        }
        Snapshot {
            version: 1,
            entries,
        }
    }

    #[test]
    fn statuses() {
        let base = snap(&[
            ("same", 100, 10),
            ("up", 100, 10),
            ("small", 100, 10),
            ("down", 100, 10),
            ("gone", 1, 1),
        ]);
        let head = snap(&[
            ("same", 100, 10),
            ("up", 106, 10),
            ("small", 105, 10),
            ("down", 90, 10),
            ("new", 5, 1),
        ]);
        let rep = diff(&base, &head, &Options::default());
        let st = |n: &str| rep.rows.iter().find(|r| r.name == n).unwrap().status;
        assert_eq!(st("same"), Status::Unchanged);
        assert_eq!(st("up"), Status::Regressed); // +6% > 5%
        assert_eq!(st("small"), Status::Changed); // +5% is not > 5%
        assert_eq!(st("down"), Status::Improved);
        assert_eq!(st("gone"), Status::Removed);
        assert_eq!(st("new"), Status::Added);
        assert!(rep.has_regression());
    }

    #[test]
    fn mixed_up_and_down_is_changed_unless_over_threshold() {
        let base = snap(&[("x", 100, 100)]);
        let head = snap(&[("x", 90, 103)]);
        let rep = diff(&base, &head, &Options::default());
        assert_eq!(rep.rows[0].status, Status::Changed);
    }

    #[test]
    fn zero_base() {
        assert_eq!(pct_change(0, 0), Some(0.0));
        assert_eq!(pct_change(0, 5), None);
        assert!(is_regression(0, 1, 1000.0));
        assert!(!is_regression(0, 0, 0.0));
    }

    #[test]
    fn zero_threshold_flags_any_increase() {
        let rep = diff(
            &snap(&[("x", 100, 1)]),
            &snap(&[("x", 101, 1)]),
            &Options {
                threshold_pct: 0.0,
                ..Options::default()
            },
        );
        assert!(rep.has_regression());
    }

    #[test]
    fn only_selected_metrics_decide() {
        let rep = diff(
            &snap(&[("x", 100, 1)]),
            &snap(&[("x", 100, 1000)]),
            &Options {
                metrics: vec![Metric::Instructions],
                threshold_pct: 5.0,
                fail_on_removed: false,
            },
        );
        assert_eq!(rep.rows[0].status, Status::Changed);
    }

    #[test]
    fn huge_values_do_not_panic() {
        let rep = diff(
            &snap(&[("x", u64::MAX, u64::MAX)]),
            &snap(&[("x", 1, 0)]),
            &Options::default(),
        );
        assert_eq!(rep.rows[0].status, Status::Improved);
        let md = render_markdown(&rep, &Options::default(), true, 10);
        assert!(md.contains("18,446,744,073,709,551,615"));
    }

    #[test]
    fn metric_parsing() {
        assert_eq!(Metric::parse_list("all").unwrap().len(), 8);
        assert_eq!(
            Metric::parse_list("instructions, fee_stroops,instructions").unwrap(),
            vec![Metric::Instructions, Metric::FeeStroops]
        );
        assert!(Metric::parse_list("gas").is_err());
        assert!(Metric::parse_list(" , ").is_err());
    }

    #[test]
    fn validate_threshold() {
        for t in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(validate(&Options {
                threshold_pct: t,
                ..Options::default()
            })
            .is_err());
        }
        assert!(validate(&Options::default()).is_ok());
    }

    #[test]
    fn markdown_shape() {
        let rep = diff(
            &snap(&[("a", 100, 10), ("b", 1, 1)]),
            &snap(&[("a", 200, 10), ("b", 1, 1)]),
            &Options::default(),
        );
        let md = render_markdown(&rep, &Options::default(), false, 50);
        assert!(md.contains("**1 regression(s)**"));
        assert!(md.contains("| `a` | 100 → 200 (+100.0%) | 10 | 🔴 regression |"));
        assert!(!md.contains("`b`"));
        let md_all = render_markdown(&rep, &Options::default(), true, 50);
        assert!(md_all.contains("`b`"));
    }

    #[test]
    fn markdown_truncates() {
        let items: Vec<(String, u64, u64)> =
            (0..30).map(|i| (format!("e{i:02}"), i + 1, 1)).collect();
        let refs: Vec<(&str, u64, u64)> =
            items.iter().map(|(n, a, b)| (n.as_str(), *a, *b)).collect();
        let rep = diff(&Snapshot::default(), &snap(&refs), &Options::default());
        let md = render_markdown(&rep, &Options::default(), false, 10);
        assert!(md.contains("… and 20 more rows not shown."));
    }

    #[test]
    fn invalid_names_never_rendered_raw() {
        // Construct a snapshot directly (bypassing validation) with a hostile name.
        let mut entries = BTreeMap::new();
        entries.insert(
            "x|<img src=x onerror=alert(1)>".to_string(),
            Entry::default(),
        );
        let s = Snapshot {
            version: 1,
            entries,
        };
        let rep = diff(&Snapshot::default(), &s, &Options::default());
        let md = render_markdown(&rep, &Options::default(), true, 10);
        assert!(!md.contains("<img"));
        assert!(md.contains("<invalid name>"));
        let txt = render_text(&rep, &Options::default(), true);
        assert!(!txt.contains("<img"));
    }

    #[test]
    fn exact_threshold_boundary() {
        // exactly +7% is not a regression at threshold 7
        assert!(!is_regression(300, 321, 7.0));
        assert!(is_regression(300, 322, 7.0));
        // precision above 2^53
        let b = 1u64 << 60;
        assert!(is_regression(b, b + 1, 0.0));
        assert!(!is_regression(u64::MAX - 1, u64::MAX, 1.0));
        assert!(is_regression(1, u64::MAX, MAX_THRESHOLD_PCT));
    }

    #[test]
    fn display_never_understates_increase() {
        assert_eq!(fmt_pct(10000, 10504), "+5.1%");
        assert_eq!(fmt_pct(1_000_000, 1_000_001), "+0.1%");
        assert_eq!(fmt_pct(1000, 999), "-0.1%");
        assert_eq!(fmt_pct(100, 100), "0%");
        assert_eq!(fmt_pct(0, 5), "new");
        assert_eq!(trim_float(-0.0), "0");
        let big = 1u64 << 60;
        assert_eq!(fmt_pct(big + 1, big), "-0.1%");
        assert_eq!(fmt_pct(big, big + 1), "+0.1%");
    }

    #[test]
    fn removed_entries_can_fail() {
        let base = snap(&[("a", 1, 1), ("b", 1, 1)]);
        let head = snap(&[("a", 1, 1)]);
        let lax = Options::default();
        assert!(!diff(&base, &head, &lax).failed(&lax));
        let strict = Options {
            fail_on_removed: true,
            ..Options::default()
        };
        let rep = diff(&base, &head, &strict);
        assert!(rep.failed(&strict));
        assert!(render_markdown(&rep, &strict, false, 10).contains("1 entry missing"));
    }

    #[test]
    fn threshold_upper_bound() {
        assert!(validate(&Options {
            threshold_pct: MAX_THRESHOLD_PCT + 1.0,
            ..Options::default()
        })
        .is_err());
    }

    #[test]
    fn num_format() {
        assert_eq!(fmt_num(0), "0");
        assert_eq!(fmt_num(999), "999");
        assert_eq!(fmt_num(1000), "1,000");
        assert_eq!(fmt_num(1234567), "1,234,567");
    }
}
