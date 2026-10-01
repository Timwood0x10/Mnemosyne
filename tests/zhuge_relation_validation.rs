//! Faction-constraint validation for the `faction` module.

use mnemosyne::faction;

/// Objective: Verify the faction constraint gate accepts a same-faction pair,
/// rejects a cross-faction pair, and applies the documented score modifier.
/// Invariants: all three names resolve to their catalogued faction; only the
/// same-faction pair passes `same_faction_or_unknown`; the bonus is 1.2 for the
/// same-faction pair and 0.8 for the cross-faction pair.
#[tokio::test]
async fn test_faction_constraint_logic() {
    assert_eq!(
        faction::get_faction("三国演义", "刘备"),
        Some("蜀"),
        "刘备 must resolve to the 蜀 faction"
    );
    assert_eq!(
        faction::get_faction("三国演义", "鲁肃"),
        Some("吴"),
        "鲁肃 must resolve to the 吴 faction"
    );
    assert_eq!(
        faction::get_faction("三国演义", "诸葛亮"),
        Some("蜀"),
        "诸葛亮 must resolve to the 蜀 faction"
    );

    assert!(
        faction::same_faction_or_unknown("三国演义", "刘备", "诸葛亮"),
        "two 蜀 characters must pass the same-faction gate"
    );
    assert!(
        !faction::same_faction_or_unknown("三国演义", "诸葛亮", "鲁肃"),
        "a 蜀/吴 pair must be rejected by the same-faction gate"
    );

    let bonus_same = faction::faction_bonus("三国演义", "刘备", "诸葛亮")
        .expect("a catalogued same-faction pair must yield a bonus");
    let bonus_diff = faction::faction_bonus("三国演义", "诸葛亮", "鲁肃")
        .expect("a catalogued cross-faction pair must yield a bonus");

    assert_eq!(
        bonus_same, 1.2,
        "a same-faction pair must be boosted to 1.2"
    );
    assert_eq!(
        bonus_diff, 0.8,
        "a cross-faction pair must be damped to 0.8"
    );
}
