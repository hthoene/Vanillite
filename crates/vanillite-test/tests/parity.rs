//! Scenario parity with Vanilla: every scenario runs serially and in parallel (verify mode) and
//! its trace must equal the golden exactly.

use vanillite_test::runner::{check_quick, check_tier};
use vanillite_test::scenario::Tier;

// The scenario world: a headless game level, or the server for scenarios across dimensions.
const WORLD: vanillite_test::world::WorldFactory = vanillite_test::server_world::factory;

#[test]
fn fast_scenarios() {
    let failures = check_tier(Tier::Fast, WORLD);
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
#[ignore = "bulk tier: runs nightly with the goldens cache"]
fn bulk_scenarios() {
    let failures = check_tier(Tier::Bulk, WORLD);
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}

#[test]
#[ignore = "quick tier: a subset of fast_scenarios for the quick check (cargo xtask parity quick)"]
fn quick_scenarios() {
    let failures = check_quick(WORLD);
    assert!(failures.is_empty(), "\n{}", failures.join("\n\n"));
}
