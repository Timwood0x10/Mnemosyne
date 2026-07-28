use lore_scope::ingest::relation::detect_relation_type;

fn main() {
    let cases = vec![
        ("诸葛亮为丞相，刘备为主公", "诸葛亮", "刘备", "君臣"),
        ("刘备、关羽、张飞三人结义为兄弟", "刘备", "关羽", "结义"),
        ("宋江与扈三娘配为夫妇", "宋江", "扈三娘", "夫妻"),
        (
            "孔明曰：主公若欲亮行兵，乞假剑印。玄德曰",
            "诸葛亮",
            "刘备",
            "君臣",
        ),
    ];
    for (ctx, a, b, expected) in cases {
        let r = detect_relation_type(ctx, a, b);
        let ok = if r == expected { "✓" } else { "✗" };
        println!("{ok} detect({a},{b}) = {r} (expected {expected})  ctx: {ctx}");
    }
}
