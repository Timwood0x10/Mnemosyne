//! 直接写 DB：写入阵营数据 + 阵营关系。
//! Run: cargo test --test faction_db write_faction_to_db -- --nocapture

use lore_scope::knowledge::{KnowledgeStore, SQLiteKnowledgeStore};
use std::sync::Arc;

const DB_PATH: &str = "/tmp/lorescope_sanguo.db";

#[tokio::test]
async fn write_faction_to_db() {
    eprintln!("\n========== 阵营数据写入 DB ==========\n");

    let k = Arc::new(SQLiteKnowledgeStore::open(DB_PATH).await.unwrap());

    // 打开 faction_map.json
    let content = std::fs::read_to_string("config/faction_map.json").unwrap();
    let map: std::collections::HashMap<String, std::collections::HashMap<String, Vec<String>>> =
        serde_json::from_str(&content).unwrap();

    if let Some(factions) = map.get("三国演义") {
        for (faction, members) in factions {
            for name in members {
                // 找到或创建 entity
                if let Some(entity) = k.find_object_by_name(name, None).await.unwrap() {
                    let profile_json = serde_json::json!({"faction": faction});
                    let conn = rusqlite::Connection::open(DB_PATH).unwrap();
                    conn.execute(
                        "UPDATE knowledge_objects SET properties = ?1 WHERE id = ?2",
                        rusqlite::params![profile_json.to_string(), entity.id],
                    ).unwrap();
                    eprintln!("  {} → {}", name, faction);
                }
            }
        }
    }

    eprintln!("\n阵营数据写入完成。开始验证...\n");

    // 验证几个角色
    for name in &["刘备", "曹操", "孙权", "吕布"] {
        if let Some(entity) = k.find_object_by_name(name, None).await.unwrap() {
            let props_str = entity.properties.to_string();
            eprintln!("  {}: properties={}", name, props_str);
        }
    }

    eprintln!("\n========== 完成 ==========");
}
