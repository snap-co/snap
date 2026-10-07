//! Configuration help uses the same descriptors as runtime selection.
use snap_platform_tests::configuration::{Carrier, Host, Storage};
use std::fmt::Write;

pub(super) fn details() -> String {
    let mut text = String::from(
        "Examples:
  ./bin/test                             Default: all, fast matrix, 4 jobs
  ./bin/test all --matrix full --jobs 8   All implemented configurations
  ./bin/test core --verbose              Core contracts with setup details
  ./bin/test interface --matrix full --list
                                        Preview ownership and selections
  ./bin/test io --timeout 60             Ignored integration contracts
  ./bin/test all --cold --keep           Retain a fresh build for inspection

Native composition configurations, each exercising client + server SDK:
",
    );
    for storage in Storage::ALL {
        for carrier in Carrier::ALL {
            let host = Host { storage, carrier };
            writeln!(
                text,
                "  {:<16} / {:<22} {}",
                storage.name(),
                carrier.name(),
                if host.fast() {
                    "fast + full"
                } else {
                    "full only"
                }
            )
            .expect("writing to a String cannot fail");
        }
    }
    text.push_str(
        "
Selection and applicability:
  --matrix selects setups, not additional cases. There are no individual
  Store/carrier filters. Store-only and carrier-only contracts stay independent.
  Fast mode also omits interchangeable file-SQLite Store and Document manifest
  rows; full mode includes them. Adapter-specific checks retain their setups.
  Process-crash durability applies only to file-backed SQLite. Memory and
  SQLite in memory report N/A. This does not prove power-loss durability.
  Atomic transactions and isolation are mandatory and cannot be disabled.
  Only all/check run static checks; only all/test run doctests.
  --list reads metadata without compiling test suites or running checks.
  The wrapper still bootstraps the CLI. Exact cases are discovered at runtime.

Results and execution:
  Default output groups contracts by layer and owner. --verbose expands
  configurations and streams commands/output; failures expand automatically.
  PASS means every selected native case passed. NOT SELECTED means excluded
  by the matrix; N/A means an optional guarantee is unsupported. Missing
  implementations are coverage gaps, not passing or N/A cases.
  Commands, stdout/stderr, browser artifacts and summary.json remain under
  Cargo's target directory in verification/run-*. Successful chatter stays
  in logs unless verbose. Failures return nonzero; cancellation preserves
  signal exit status and retires owned subprocess groups.
  Builds share the workspace Cargo cache; --jobs bounds execution, not builds.
  --timeout retires a contract's process group and records failure.
  --cold uses $HOME/.cache/coding-agents without replacing the normal cache.
  Cold artifacts are removed on success unless --keep; retained on failure.

Current coverage limits:
  The nine native compositions share one portable cartridge journey.
  Core host variation covers Document manifest holdings across three Stores;
  other core contracts retain their existing setups. Shared Wasm conformance,
  browser client-carrier conformance and client-side durable recovery are gaps.

Environment:
  Use the toolchain and dependency-tool pins in mise.toml. Static checks
  need Bun and installed workspace JS dependencies. Browser suites need
  Chromium on PATH, or SNAP_BROWSER pointing to an installed executable.
  Dependency policy checks unused dependencies, bans and sources, not advisories.
  For application suites, use snap test <app>; bin/test is framework-only.
",
    );
    text
}
