//! Scenario parity with Vanilla: every scenario runs serially and in parallel (verify mode) and
//! its trace must equal the golden exactly.

use vanillite_test::runner::check_tier;
use vanillite_test::scenario::Tier;
use vanillite_test::world::FlatWorld;

// The scenario world. `FlatWorld` stands in until `World::headless` (P6) exists.
const WORLD: vanillite_test::world::WorldFactory = FlatWorld::factory;

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
