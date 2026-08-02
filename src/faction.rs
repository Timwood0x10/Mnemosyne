use std::collections::HashMap;
use std::sync::LazyLock;

/// Faction map: novel → (faction name, member list).
///
/// The inner faction key is a leaked `&'static str`: the map is a
/// process-lifetime config cache (`LazyLock`), so each faction name is
/// leaked exactly ONCE at load time. This replaces the old per-lookup
/// `unsafe transmute::<&str, &'static str>` (NEW-I2), which was fragile —
/// any future refactor that shortened the borrow could turn it into UB.
/// `Box::leak` is safe and makes the `'static` guarantee structural.
type FactionMap = HashMap<String, HashMap<&'static str, Vec<String>>>;

fn load_faction_map() -> FactionMap {
    let path =
        std::env::var("FACTION_MAP_PATH").unwrap_or_else(|_| "config/faction_map.json".to_string());
    // Parse with owned keys first, then leak the faction names once.
    let parsed: HashMap<String, HashMap<String, Vec<String>>> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    parsed
        .into_iter()
        .map(|(novel, factions)| {
            let factions = factions
                .into_iter()
                .map(|(faction, members)| {
                    // Box::leak returns &'static mut str; reborrow as the
                    // immutable &'static str the map stores.
                    let faction: &'static str = Box::leak(faction.into_boxed_str());
                    (faction, members)
                })
                .collect();
            (novel, factions)
        })
        .collect()
}

static FACTION_MAP: LazyLock<FactionMap> = LazyLock::new(load_faction_map);

fn faction_from_json(novel: &str, character: &str) -> Option<&'static str> {
    for (faction, members) in FACTION_MAP.get(novel)? {
        if members.iter().any(|m| m == character) {
            // Safe: faction names were leaked to 'static at load time.
            return Some(*faction);
        }
    }
    None
}

/// Novel-specific faction lookup.
///
/// Checks the JSON config file first (`config/faction_map.json`), then
/// falls back to the hardcoded built-in map.
pub fn get_faction(novel: &str, character: &str) -> Option<&'static str> {
    if let Some(f) = faction_from_json(novel, character) {
        return Some(f);
    }
    hardcoded_faction(novel, character)
}

/// Legacy hardcoded faction map, used when no JSON config is found.
fn hardcoded_faction(novel: &str, character: &str) -> Option<&'static str> {
    match (novel, character) {
        (
            "三国演义",
            "刘备" | "关羽" | "张飞" | "诸葛亮" | "赵云" | "马超" | "黄忠" | "姜维" | "庞统"
            | "法正" | "魏延" | "刘禅",
        ) => Some("蜀"),
        (
            "三国演义",
            "曹操" | "司马懿" | "张辽" | "徐晃" | "夏侯惇" | "夏侯渊" | "曹仁" | "许褚" | "典韦"
            | "郭嘉" | "荀彧" | "程昱" | "贾诩" | "曹丕" | "司马昭" | "邓艾",
        ) => Some("魏"),
        (
            "三国演义",
            "孙权" | "周瑜" | "鲁肃" | "吕蒙" | "陆逊" | "黄盖" | "甘宁" | "太史慈" | "张昭",
        ) => Some("吴"),
        (
            "三国演义",
            "董卓" | "吕布" | "袁绍" | "袁术" | "刘表" | "刘璋" | "马腾" | "公孙瓒" | "高顺"
            | "陈宫" | "颜良" | "文丑" | "华雄",
        ) => Some("群雄"),
        (
            "水浒传",
            "宋江" | "卢俊义" | "吴用" | "武松" | "林冲" | "鲁智深" | "李逵" | "花荣" | "柴进"
            | "杨志" | "呼延灼" | "秦明" | "关胜" | "晁盖" | "公孙胜" | "燕青" | "戴宗" | "张顺"
            | "石秀" | "阮小七",
        ) => Some("梁山"),
        ("水浒传", "高俅" | "蔡京" | "童贯" | "宋徽宗") => Some("朝廷"),
        ("西游记", "唐僧" | "孙悟空" | "猪八戒" | "沙僧" | "白龙马") => {
            Some("取经")
        }
        ("西游记", "如来" | "观音" | "文殊" | "普贤") => Some("佛派"),
        ("西游记", "玉帝" | "太上老君" | "王母") => Some("天庭"),
        (
            "红楼梦",
            "贾宝玉" | "林黛玉" | "薛宝钗" | "贾母" | "王熙凤" | "贾政" | "贾琏" | "元春" | "探春",
        ) => Some("贾府"),
        ("红楼梦", "薛蟠" | "薛姨妈") => Some("薛家"),
        ("红楼梦", "王夫人") => Some("王家"),
        _ => None,
    }
}

pub fn faction_bonus(novel: &str, a: &str, b: &str) -> Option<f64> {
    match (get_faction(novel, a), get_faction(novel, b)) {
        (Some(fa), Some(fb)) => {
            if fa == fb {
                Some(1.2)
            } else {
                Some(0.8)
            }
        }
        _ => None,
    }
}

pub fn same_faction_or_unknown(novel: &str, a: &str, b: &str) -> bool {
    match (get_faction(novel, a), get_faction(novel, b)) {
        (Some(fa), Some(fb)) => fa == fb,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanguo_shu() {
        assert_eq!(get_faction("三国演义", "刘备"), Some("蜀"));
        assert_eq!(get_faction("三国演义", "诸葛亮"), Some("蜀"));
        assert_eq!(get_faction("三国演义", "关羽"), Some("蜀"));
    }

    #[test]
    fn test_sanguo_wei() {
        assert_eq!(get_faction("三国演义", "曹操"), Some("魏"));
        assert_eq!(get_faction("三国演义", "司马懿"), Some("魏"));
    }

    #[test]
    fn test_sanguo_wu() {
        assert_eq!(get_faction("三国演义", "孙权"), Some("吴"));
        assert_eq!(get_faction("三国演义", "周瑜"), Some("吴"));
    }

    #[test]
    fn test_unknown_character() {
        assert_eq!(get_faction("三国演义", "无名氏"), None);
        assert_eq!(get_faction("未知小说", "刘备"), None);
    }

    #[test]
    fn test_faction_bonus_same() {
        assert_eq!(faction_bonus("三国演义", "刘备", "诸葛亮"), Some(1.2));
    }

    #[test]
    fn test_faction_bonus_different() {
        assert_eq!(faction_bonus("三国演义", "刘备", "曹操"), Some(0.8));
    }

    #[test]
    fn test_faction_bonus_unknown() {
        assert_eq!(faction_bonus("三国演义", "刘备", "无名氏"), None);
    }
}
