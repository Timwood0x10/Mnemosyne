use lore_scope::faction;

fn main() {
    println!("=== Faction checks ===");
    println!("吕布 faction: {:?}", faction::get_faction("三国演义", "吕布"));
    println!("董卓 faction: {:?}", faction::get_faction("三国演义", "董卓"));
    println!("刘备 faction: {:?}", faction::get_faction("三国演义", "刘备"));
    println!("曹操 faction: {:?}", faction::get_faction("三国演义", "曹操"));
    println!("鲁肃 faction: {:?}", faction::get_faction("三国演义", "鲁肃"));
    
    println!("\nSame faction checks:");
    println!("吕布-董卓 same: {}", faction::same_faction_or_unknown("三国演义", "吕布", "董卓"));
    println!("吕布-刘备 same: {}", faction::same_faction_or_unknown("三国演义", "吕布", "刘备"));
}
