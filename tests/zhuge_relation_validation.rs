use lore_scope::faction;

#[tokio::test]
async fn test_faction_constraint_logic() {
    // Directly test faction constraint functions

    // Check faction lookups
    assert_eq!(faction::get_faction("三国演义", "刘备"), Some("蜀"));
    assert_eq!(faction::get_faction("三国演义", "鲁肃"), Some("吴"));
    assert_eq!(faction::get_faction("三国演义", "诸葛亮"), Some("蜀"));

    // Test same_faction_or_unknown
    assert!(faction::same_faction_or_unknown(
        "三国演义",
        "刘备",
        "诸葛亮"
    ));
    assert!(!faction::same_faction_or_unknown(
        "三国演义",
        "诸葛亮",
        "鲁肃"
    ));

    // Test faction_bonus
    let bonus_same = faction::faction_bonus("三国演义", "刘备", "诸葛亮");
    let bonus_diff = faction::faction_bonus("三国演义", "诸葛亮", "鲁肃");

    match (bonus_same, bonus_diff) {
        (Some(s), Some(d)) => {
            assert_eq!(s, 1.2);
            assert_eq!(d, 0.8);
        }
        _ => panic!("Unexpected None values"),
    }

    println!("✓ Faction constraint tests passed");
}
