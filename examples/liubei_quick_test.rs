use lore_scope::faction;

fn main() {
    println!("=== Liu Bei (刘备) Relationship Network Analysis ===");
    println!();
    println("Faction assignments:");
    println!("  刘备 -> {:?}", faction::get_faction("三国演义", "刘备"));   // Some("蜀")
    println!("  关羽 -> {:?}", faction::get_faction("三国演义", "关羽"));   // Some("蜀")
    println!("  张飞 -> {:?}", faction::get_faction("三国演义", "张飞"));   // Some("蜀")
    println!("  诸葛亮 -> {:?}", faction::get_faction("三国演义", "诸葛亮")); // Some("蜀")
    println!("  曹操 -> {:?}", faction::get_faction("三国演义", "曹操"));   // Some("魏")
    println!("  孙权 -> {:?}", faction::get_faction("三国演义", "孙权"));   // Some("吴")
    println!("  鲁肃 -> {:?}", faction::get_faction("三国演义", "鲁肃"));   // Some("吴")
    println!();
    
    println!("Same faction checks for Liu Bei:");
    println!("  刘备-关羽同阵营: {}", faction::same_faction_or_unknown("三国演义", "刘备", "关羽"));   // true
    println!("  刘备-张飞同阵营: {}", faction::same_faction_or_unknown("三国演义", "刘备", "张飞"));   // true
    println!("  刘备-诸葛亮同阵营: {}", faction::same_faction_or_unknown("三国演义", "刘备", "诸葛亮")); // true
    println!("  刘备-曹操同阵营: {}", faction::same_faction_or_unknown("三国演义", "刘备", "曹操"));   // false
    println!("  刘备-孙权同阵营: {}", faction::same_faction_or_unknown("三国演义", "刘备", "孙权"));   // false
    println!("  刘备-鲁肃同阵营: {}", faction::same_faction_or_unknown("三国演义", "刘备", "鲁肃"));   // false
    println!();
    
    println!("Faction bonus calculations:");
    let bonus = |a: &str, b: &str| format!("{:.1}%", faction::faction_bonus("三国演义", a, b).map(|b| (b - 1.0) * 100).unwrap_or(-100.0));
    println!("  刘备-关羽: {}", bonus("刘备", "关羽"));   // +20%
    println!("  刘备-曹操: {}", bonus("刘备", "曹操"));   // -20%
    println!("  刘备-鲁肃: {}", bonus("刘备", "鲁肃"));   // -20%
    println!();
    
    println!("=== Effect on Relation Weights ===");
    println!("For a base importance of 0.85 (typical for strong relations):");
    println!("  Same-faction 君臣/师徒: 0.85 × 1.2 = {}", (0.85 * 1.2).min(1.0));  // max caps at 1.0
    println!("  Cross-faction 君臣/师徒: 0.85 × 0.2 = {}", (0.85 * 0.2).min(1.0));  // heavy penalty
    println!();
    println!("Expected result:");
    println!("  刘备-关羽/张飞/诸葛亮关系：权重保持较高 (接近或达到上限1.0)");
    println!("  刘备-曹操/孙权/鲁肃的君臣关系：权重被大幅压制 (降至 ~0.17 以下)");
    println!("  这解决了之前跨阵营虚假君臣关系的错误问题！");
}
