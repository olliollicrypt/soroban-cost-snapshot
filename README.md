# soroban-cost-snapshot

Per-function resource-cost snapshots for Soroban (Stellar) smart contracts —
like Foundry's gas snapshots, but for Soroban.

Record what each contract call costs in your tests, commit that as a
baseline, and let CI tell you when a pull request makes a call more
expensive:

| Entry | CPU instructions | Est. fee (stroops) | Status |
|---|---:|---:|---|
| `counter::incr` | 19,900 → 58,540 (+194.2%) | 10,376 → 10,403 (+0.3%) | 🔴 regression |

_(Output for the demo contract in `examples/demo-contract`, where the `slow`
feature simulates a pull request that re-reads storage in a loop.)_

Why: Soroban transactions have hard resource limits (CPU instructions,
memory, ledger reads/writes) and fees follow resource use. Unit tests check
that code *works*, not what it *costs*, so cost regressions slip through
review.

> Built by an AI agent, operated by a private individual in Belgium.
> Status: early (v0.1). Feedback and issues welcome.

## How it works

1. In a test, after a contract call, add one line:

   ```rust
   use soroban_cost_snapshot::cost_snapshot;

   #[test]
   fn costs() {
       let env = Env::default();
       let id = env.register(Counter, ());
       let c = CounterClient::new(&env, &id);

       c.incr(&5);
       cost_snapshot!(env, "counter::incr");
   }
   ```

   The macro reads `env.cost_estimate()` (from your own `soroban-sdk`) for the
   last top-level invocation and writes one small JSON file.

2. Create the baseline and commit it:

   ```sh
   cargo install --git https://github.com/olliollicrypt/soroban-cost-snapshot soroban-cost-snapshot
   rm -rf target/soroban-cost-snapshot && cargo test && soroban-cost collect --out .cost-snapshot.json
   git add .cost-snapshot.json
   ```

3. Compare in CI (see the GitHub Action below) or locally:

   ```sh
   soroban-cost diff .cost-snapshot.json new.json --threshold 5 --format markdown
   ```

   Exit code `0` = ok, `1` = a selected metric rose by more than the
   threshold (or, with `--fail-on-removed`, an entry of the baseline is
   missing), `2` = usage or input error (also: the new snapshot has no
   entries while the baseline has some).

When a cost increase is intended, regenerate and commit the baseline in the
same pull request. The Action compares against the baseline of the PR's
**base commit**, so the check stays red until a maintainer accepts the
increase (see "Accepting an intended increase" below).

## Setup

```toml
[dev-dependencies]
soroban-sdk = { version = "29", features = ["testutils"] }
soroban-cost-snapshot = { git = "https://github.com/olliollicrypt/soroban-cost-snapshot" }
```

The crate does not depend on `soroban-sdk` itself; the macro expands to code
that calls `Env::cost_estimate()` on your `Env`. Tested with soroban-sdk
29.0.1.

## Metrics

| Key | Meaning |
|---|---|
| `instructions` | modelled CPU instructions |
| `mem_bytes` | modelled memory |
| `read_entries` | ledger entries read (disk + in-memory) |
| `write_entries` | ledger entries written |
| `read_bytes` / `write_bytes` | bytes read from disk / written |
| `events_bytes` | size of emitted contract events |
| `fee_stroops` | SDK fee estimate (hardcoded mainnet fee table) |

By default `instructions` and `fee_stroops` decide pass/fail
(`--metrics all` to use all of them).

## GitHub Action

```yaml
name: contract costs
on: pull_request
permissions:
  contents: read
jobs:
  costs:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@stable
      - uses: olliollicrypt/soroban-cost-snapshot@v0.1.0   # pin to a commit SHA in production
        with:
          working-directory: contracts/my-contract
          threshold: "5"
          # metrics: "instructions,fee_stroops"
```

The action writes the table to the **job summary** and fails the check on a
regression. It needs no token and no write permissions.

| Input | Default | Meaning |
|---|---|---|
| `working-directory` | `.` | contract package directory |
| `baseline` | `.cost-snapshot.json` | baseline file, relative to `working-directory` |
| `test-command` | `cargo test` | runs the tests that call `cost_snapshot!` |
| `threshold` | `5` | allowed increase in percent (0–1,000,000) |
| `metrics` | `instructions,fee_stroops` | metrics that decide pass/fail, or `all` |
| `fail-on-regression` | `true` | only exactly `"false"` turns failing off |
| `fail-on-removed` | `true` | fail if a baseline entry is missing from this run |
| `baseline-from-base-commit` | `true` | on PRs, read the baseline from the base commit |
| `require-baseline` | `true` | fail when no baseline exists |

Outputs: `result` (`pass` / `regression` / `no-baseline`) and
`snapshot-path` (the new snapshot, e.g. to upload as an artifact).

### Accepting an intended increase

Let a maintainer-only label switch failing off for that PR (contributors
from forks cannot add labels). The workflow must also run on label events:

```yaml
on:
  pull_request:
    types: [opened, synchronize, reopened, labeled, unlabeled]
# ...
        with:
          fail-on-regression: ${{ !contains(github.event.pull_request.labels.*.name, 'cost-increase-ok') }}
```

The table still appears in the job summary, so the accepted increase is
documented in the PR.

## Accuracy and limits

- Numbers come from soroban-sdk's cost estimate inside tests. The SDK itself
  warns they are estimates: use them to spot **changes**, not as exact
  network fees. Use RPC simulation for exact values.
- Contracts registered natively (`env.register(Contract, ())`) **under-report**
  VM costs. For realistic numbers, register the compiled Wasm in your cost
  tests.
- One entry per name; recording the same name twice in one run keeps the
  last value, so use a unique name per measured call.
- `cost_snapshot!` measures the **last** top-level invocation. Calling it
  twice without a contract call in between records the same cost twice.
- Entry names: 1–96 characters from `A-Z a-z 0-9 _ . : - /`.
- Remove `target/soroban-cost-snapshot` before collecting locally, or renamed
  tests leave stale entries (the Action always uses a fresh directory).
- In a cargo workspace, each test binary runs from its own package
  directory, so entries land in `<package>/target/soroban-cost-snapshot`.
  Set `SOROBAN_COST_SNAPSHOT_DIR` to an absolute path to gather them in one
  place.
- Thresholds are compared exactly (integer math, threshold rounded to
  0.001%); displayed percentages are rounded away from zero, so an increase
  never looks smaller than it is.

## Security

- **What this check is (and is not):** it catches honest mistakes. It is not
  a security boundary against a hostile pull request: the PR's own test code
  runs in the job and could tamper with the job (as with any CI step that
  runs PR code). Keep the job's permissions minimal and review PRs as usual.
- The baseline is read from the PR's base commit by default (the job fails
  if that commit cannot be fetched — for private repos keep checkout
  credentials or use `fetch-depth: 0`), so deleting or
  editing the baseline inside a PR does not make the check pass. Deleted or
  skipped cost tests fail the check (`fail-on-removed`), and an empty run is
  an error.
- The Action runs only on the workflow's own permissions; the example uses
  `contents: read` and `pull_request` (never `pull_request_target`, which
  would run untrusted PR code with a write token).
- Action inputs are passed through environment variables, never expanded
  inside shell scripts.
- Snapshot files are size-capped (16 MiB), strictly validated (unknown
  fields, duplicate keys, invalid names and negative numbers are rejected)
  and entry names are restricted so they cannot inject Markdown or HTML into
  reports. Error messages escape control characters, so quoted file content
  cannot start a GitHub workflow command.
- See [SECURITY.md](SECURITY.md) to report a problem.

## Development

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked                     # unit, CLI and property tests
cd examples/demo-contract && cargo test --locked
```

## License

MIT — see [LICENSE](LICENSE).
