//! Property tests: random input must never panic, and output must never
//! contain characters outside the allowed name set where names are printed.

use proptest::prelude::*;
use soroban_cost_snapshot::diff::{diff, render_markdown, render_text, Metric, Options};
use soroban_cost_snapshot::{is_valid_name, Entry, Snapshot};
use std::collections::BTreeMap;

fn entry() -> impl Strategy<Value = Entry> {
    (
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
        any::<u64>(),
    )
        .prop_map(|(a, b, c, d, e, f, g, h)| Entry {
            instructions: a,
            mem_bytes: b,
            read_entries: c,
            write_entries: d,
            read_bytes: e,
            write_bytes: f,
            events_bytes: g,
            fee_stroops: h,
        })
}

fn snapshot() -> impl Strategy<Value = Snapshot> {
    prop::collection::btree_map(".{0,20}", entry(), 0..20).prop_map(
        |entries: BTreeMap<String, Entry>| Snapshot {
            version: 1,
            entries,
        },
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn from_json_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = Snapshot::from_json(&bytes);
    }

    #[test]
    fn from_json_never_panics_on_jsonish(s in r#"\{"version":[0-9]{1,3},"entries":\{("[^"]{0,8}":\{[^}]{0,60}\})?\}\}"#) {
        let _ = Snapshot::from_json(s.as_bytes());
    }

    #[test]
    fn diff_and_render_never_panic(a in snapshot(), b in snapshot(), t in 0.0f64..1000.0, all in any::<bool>(), rows in 1usize..50) {
        let opts = Options { metrics: Metric::ALL.to_vec(), threshold_pct: t, fail_on_removed: false };
        let rep = diff(&a, &b, &opts);
        let md = render_markdown(&rep, &opts, all, rows);
        let txt = render_text(&rep, &opts, all);
        // Every name that is printed must be a valid name.
        for r in &rep.rows {
            if !is_valid_name(&r.name) && !r.name.is_empty() {
                let quoted = format!("`{}`", r.name);
                prop_assert!(!md.contains(&quoted));
            }
        }
        let stripped = md.replace("<sub>", "").replace("</sub>", "").replace("<invalid name>", "");
        prop_assert!(!stripped.contains('<') && !stripped.contains('>'));
        let stripped_txt = txt.replace("<invalid name>", "");
        prop_assert!(!stripped_txt.contains('<'));
    }

    #[test]
    fn valid_snapshots_roundtrip(a in snapshot()) {
        let filtered = Snapshot {
            version: 1,
            entries: a.entries.into_iter().filter(|(k, _)| is_valid_name(k)).collect(),
        };
        let back = Snapshot::from_json(filtered.to_json().as_bytes()).unwrap();
        prop_assert_eq!(back, filtered);
    }

    #[test]
    fn identical_snapshots_have_no_regression(a in snapshot(), t in 0.0f64..100.0) {
        let opts = Options { metrics: Metric::ALL.to_vec(), threshold_pct: t, fail_on_removed: false };
        prop_assert!(!diff(&a, &a, &opts).has_regression());
    }
}
