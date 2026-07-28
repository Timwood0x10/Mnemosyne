use lore_scope::character::SQLiteCharacterStore;
use lore_scope::faction;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Faction System Verification ===");

    // Test faction lookups
    assert_eq!(faction::get_faction("三国演义", "吕布"), Some("群雄"));
    assert_eq!(faction::get_faction("三国演义", "董卓"), Some("群雄"));
    assert_eq!(faction::get_faction("三国演义", "刘备"), Some("蜀"));
    assert_eq!(faction::get_faction("三国演义", "曹操"), Some("魏"));
    assert_eq!(faction::get_faction("三国演义", "鲁肃"), Some("吴"));
    assert_eq!(faction::get_faction("三国演义", "诸葛亮"), Some("蜀"));
    println!("✓ All faction lookups correct");

    // Test faction bonuses
    let bonus = |a: &str, b: &str| faction::faction_bonus("三国演义", a, b);
    assert_eq!(bonus("吕布", "董卓"), Some(1.2));
    assert_eq!(bonus("吕布", "刘备"), Some(0.8));
    assert_eq!(bonus("诸葛亮", "刘备"), Some(1.2));
    assert_eq!(bonus("诸葛亮", "鲁肃"), Some(0.8));
    println!("✓ Faction bonuses correct");

    // Test same_faction_or_unknown
    assert!(faction::same_faction_or_unknown("三国演义", "吕布", "董卓"));
    assert!(!faction::same_faction_or_unknown("三国演义", "吕布", "刘备"));
    assert!(faction::same_faction_or_unknown("三国演义", "张飞", "赵云"));
    println!("✓ same_faction_or_unknown correct");

    println!("\n=== Verification Complete ===");
    println!("The faction constraint system is correctly implemented.");
    println!("After running full ingestion, cross-faction '君臣' relationships will be downranked.");
    
    Ok(())
}
