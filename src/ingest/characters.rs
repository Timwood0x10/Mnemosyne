/// Static character definitions for the four classical Chinese novels.
use std::collections::HashMap;

/// A character definition — canonical name + known aliases.
#[derive(Debug, Clone)]
pub struct CharacterDef {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
}

static SHUIHU: &[CharacterDef] = &[
    CharacterDef {
        name: "宋江",
        aliases: &["及时雨", "孝义黑三郎", "呼保义", "宋公明", "公明", "押司"],
    },
    CharacterDef {
        name: "卢俊义",
        aliases: &["玉麒麟", "卢员外"],
    },
    CharacterDef {
        name: "吴用",
        aliases: &["智多星", "加亮先生", "吴学究"],
    },
    CharacterDef {
        name: "公孙胜",
        aliases: &["入云龙", "一清先生"],
    },
    CharacterDef {
        name: "关胜",
        aliases: &["大刀", "关大刀"],
    },
    CharacterDef {
        name: "林冲",
        aliases: &["豹子头", "小张飞", "林教头"],
    },
    CharacterDef {
        name: "秦明",
        aliases: &["霹雳火"],
    },
    CharacterDef {
        name: "呼延灼",
        aliases: &["双鞭"],
    },
    CharacterDef {
        name: "花荣",
        aliases: &["小李广", "花知寨"],
    },
    CharacterDef {
        name: "柴进",
        aliases: &["小旋风", "柴大官人"],
    },
    CharacterDef {
        name: "李应",
        aliases: &["扑天雕"],
    },
    CharacterDef {
        name: "朱仝",
        aliases: &["美髯公"],
    },
    CharacterDef {
        name: "鲁智深",
        aliases: &["花和尚", "鲁达", "提辖"],
    },
    CharacterDef {
        name: "武松",
        aliases: &["行者", "武二郎", "武都头"],
    },
    CharacterDef {
        name: "董平",
        aliases: &["双枪将"],
    },
    CharacterDef {
        name: "张清",
        aliases: &["没羽箭"],
    },
    CharacterDef {
        name: "杨志",
        aliases: &["青面兽", "杨制使"],
    },
    CharacterDef {
        name: "徐宁",
        aliases: &["金枪手"],
    },
    CharacterDef {
        name: "索超",
        aliases: &["急先锋"],
    },
    CharacterDef {
        name: "戴宗",
        aliases: &["神行太保", "戴院长"],
    },
    CharacterDef {
        name: "刘唐",
        aliases: &["赤发鬼"],
    },
    CharacterDef {
        name: "李逵",
        aliases: &["黑旋风", "铁牛"],
    },
    CharacterDef {
        name: "史进",
        aliases: &["九纹龙", "史大郎"],
    },
    CharacterDef {
        name: "穆弘",
        aliases: &["没遮拦"],
    },
    CharacterDef {
        name: "雷横",
        aliases: &["插翅虎", "雷都头"],
    },
    CharacterDef {
        name: "李俊",
        aliases: &["混江龙"],
    },
    CharacterDef {
        name: "阮小二",
        aliases: &["立地太岁"],
    },
    CharacterDef {
        name: "张横",
        aliases: &["船火儿"],
    },
    CharacterDef {
        name: "阮小五",
        aliases: &["短命二郎"],
    },
    CharacterDef {
        name: "张顺",
        aliases: &["浪里白跳", "浪里白条"],
    },
    CharacterDef {
        name: "阮小七",
        aliases: &["活阎罗"],
    },
    CharacterDef {
        name: "杨雄",
        aliases: &["病关索"],
    },
    CharacterDef {
        name: "石秀",
        aliases: &["拼命三郎"],
    },
    CharacterDef {
        name: "解珍",
        aliases: &["两头蛇"],
    },
    CharacterDef {
        name: "解宝",
        aliases: &["双尾蝎"],
    },
    CharacterDef {
        name: "燕青",
        aliases: &["浪子"],
    },
    CharacterDef {
        name: "朱武",
        aliases: &["神机军师"],
    },
    CharacterDef {
        name: "黄信",
        aliases: &["镇三山"],
    },
    CharacterDef {
        name: "孙立",
        aliases: &["病尉迟"],
    },
    CharacterDef {
        name: "宣赞",
        aliases: &["丑郡马"],
    },
    CharacterDef {
        name: "郝思文",
        aliases: &["井木犴"],
    },
    CharacterDef {
        name: "韩滔",
        aliases: &["百胜将"],
    },
    CharacterDef {
        name: "单廷珪",
        aliases: &["圣水将"],
    },
    CharacterDef {
        name: "魏定国",
        aliases: &["神火将"],
    },
    CharacterDef {
        name: "王英",
        aliases: &["矮脚虎", "王矮虎"],
    },
    CharacterDef {
        name: "扈三娘",
        aliases: &["一丈青"],
    },
    CharacterDef {
        name: "鲍旭",
        aliases: &["丧门神"],
    },
    CharacterDef {
        name: "樊瑞",
        aliases: &["混世魔王"],
    },
    CharacterDef {
        name: "孔明",
        aliases: &["毛头星"],
    },
    CharacterDef {
        name: "孔亮",
        aliases: &["独火星"],
    },
    CharacterDef {
        name: "项充",
        aliases: &["八臂哪吒"],
    },
    CharacterDef {
        name: "李衮",
        aliases: &["飞天大圣"],
    },
    CharacterDef {
        name: "马麟",
        aliases: &["铁笛仙"],
    },
    CharacterDef {
        name: "童威",
        aliases: &["出洞蛟"],
    },
    CharacterDef {
        name: "童猛",
        aliases: &["翻江蜃"],
    },
    CharacterDef {
        name: "孟康",
        aliases: &["玉幡竿"],
    },
    CharacterDef {
        name: "侯健",
        aliases: &["通臂猿"],
    },
    CharacterDef {
        name: "陈达",
        aliases: &["跳涧虎"],
    },
    CharacterDef {
        name: "杨春",
        aliases: &["白花蛇"],
    },
    CharacterDef {
        name: "郑天寿",
        aliases: &["白面郎君"],
    },
    CharacterDef {
        name: "陶宗旺",
        aliases: &["九尾龟"],
    },
    CharacterDef {
        name: "宋清",
        aliases: &["铁扇子"],
    },
    CharacterDef {
        name: "乐和",
        aliases: &["铁叫子"],
    },
    CharacterDef {
        name: "施恩",
        aliases: &["金眼彪"],
    },
    CharacterDef {
        name: "李忠",
        aliases: &["打虎将"],
    },
    CharacterDef {
        name: "周通",
        aliases: &["小霸王"],
    },
    CharacterDef {
        name: "汤隆",
        aliases: &["金钱豹子"],
    },
    CharacterDef {
        name: "杜兴",
        aliases: &["鬼脸儿"],
    },
    CharacterDef {
        name: "邹渊",
        aliases: &["出林龙"],
    },
    CharacterDef {
        name: "邹润",
        aliases: &["独角龙"],
    },
    CharacterDef {
        name: "朱贵",
        aliases: &["旱地忽律"],
    },
    CharacterDef {
        name: "朱富",
        aliases: &["笑面虎"],
    },
    CharacterDef {
        name: "蔡福",
        aliases: &["铁臂膊"],
    },
    CharacterDef {
        name: "蔡庆",
        aliases: &["一枝花"],
    },
    CharacterDef {
        name: "李立",
        aliases: &["催命判官"],
    },
    CharacterDef {
        name: "李云",
        aliases: &["青眼虎"],
    },
    CharacterDef {
        name: "焦挺",
        aliases: &["没面目"],
    },
    CharacterDef {
        name: "石勇",
        aliases: &["石将军"],
    },
    CharacterDef {
        name: "孙新",
        aliases: &["小尉迟"],
    },
    CharacterDef {
        name: "顾大嫂",
        aliases: &["母大虫"],
    },
    CharacterDef {
        name: "张青",
        aliases: &["菜园子"],
    },
    CharacterDef {
        name: "孙二娘",
        aliases: &["母夜叉"],
    },
    CharacterDef {
        name: "白胜",
        aliases: &["白日鼠"],
    },
    CharacterDef {
        name: "时迁",
        aliases: &["鼓上蚤"],
    },
    CharacterDef {
        name: "晁盖",
        aliases: &["晁天王"],
    },
    CharacterDef {
        name: "王伦",
        aliases: &["白衣秀士"],
    },
    CharacterDef {
        name: "高俅",
        aliases: &["高太尉"],
    },
    CharacterDef {
        name: "潘金莲",
        aliases: &[],
    },
    CharacterDef {
        name: "西门庆",
        aliases: &[],
    },
    CharacterDef {
        name: "蒋门神",
        aliases: &[],
    },
];

static SANGUO: &[CharacterDef] = &[
    CharacterDef {
        name: "刘备",
        aliases: &["玄德", "刘皇叔", "刘玄德", "先主", "刘豫州"],
    },
    CharacterDef {
        name: "关羽",
        aliases: &["关公", "关云长", "云长", "美髯公", "关将军"],
    },
    CharacterDef {
        name: "张飞",
        aliases: &["翼德", "张翼德"],
    },
    CharacterDef {
        name: "诸葛亮",
        aliases: &["孔明", "卧龙", "诸葛丞相"],
    },
    CharacterDef {
        name: "曹操",
        aliases: &["孟德", "曹孟德", "曹阿瞒", "曹丞相", "曹公"],
    },
    CharacterDef {
        name: "赵云",
        aliases: &["赵子龙", "子龙", "常山赵子龙"],
    },
    CharacterDef {
        name: "马超",
        aliases: &["孟起", "锦马超"],
    },
    CharacterDef {
        name: "黄忠",
        aliases: &["汉升", "老将黄忠"],
    },
    CharacterDef {
        name: "魏延",
        aliases: &["文长"],
    },
    CharacterDef {
        name: "姜维",
        aliases: &["伯约", "姜伯约"],
    },
    CharacterDef {
        name: "庞统",
        aliases: &["士元", "凤雏"],
    },
    CharacterDef {
        name: "刘禅",
        aliases: &["阿斗", "后主"],
    },
    CharacterDef {
        name: "徐庶",
        aliases: &["元直"],
    },
    CharacterDef {
        name: "法正",
        aliases: &["孝直"],
    },
    CharacterDef {
        name: "马谡",
        aliases: &["幼常"],
    },
    CharacterDef {
        name: "曹丕",
        aliases: &["子桓", "魏文帝"],
    },
    CharacterDef {
        name: "夏侯惇",
        aliases: &["元让"],
    },
    CharacterDef {
        name: "夏侯渊",
        aliases: &["妙才"],
    },
    CharacterDef {
        name: "典韦",
        aliases: &[],
    },
    CharacterDef {
        name: "许褚",
        aliases: &["虎痴"],
    },
    CharacterDef {
        name: "张辽",
        aliases: &["文远"],
    },
    CharacterDef {
        name: "徐晃",
        aliases: &["公明"],
    },
    CharacterDef {
        name: "张郃",
        aliases: &["儁乂"],
    },
    CharacterDef {
        name: "曹仁",
        aliases: &["子孝"],
    },
    CharacterDef {
        name: "庞德",
        aliases: &["令明"],
    },
    CharacterDef {
        name: "贾诩",
        aliases: &["文和"],
    },
    CharacterDef {
        name: "郭嘉",
        aliases: &["奉孝"],
    },
    CharacterDef {
        name: "荀彧",
        aliases: &["文若"],
    },
    CharacterDef {
        name: "荀攸",
        aliases: &["公达"],
    },
    CharacterDef {
        name: "程昱",
        aliases: &["仲德"],
    },
    CharacterDef {
        name: "司马懿",
        aliases: &["仲达"],
    },
    CharacterDef {
        name: "孙权",
        aliases: &["仲谋", "孙仲谋", "吴侯", "吴主"],
    },
    CharacterDef {
        name: "周瑜",
        aliases: &["公瑾", "周郎"],
    },
    CharacterDef {
        name: "陆逊",
        aliases: &["伯言"],
    },
    CharacterDef {
        name: "吕蒙",
        aliases: &["子明", "吕子明"],
    },
    CharacterDef {
        name: "黄盖",
        aliases: &["公覆"],
    },
    CharacterDef {
        name: "甘宁",
        aliases: &["兴霸"],
    },
    CharacterDef {
        name: "太史慈",
        aliases: &["子义"],
    },
    CharacterDef {
        name: "鲁肃",
        aliases: &["子敬"],
    },
    CharacterDef {
        name: "孙策",
        aliases: &["伯符", "小霸王"],
    },
    CharacterDef {
        name: "袁绍",
        aliases: &["本初"],
    },
    CharacterDef {
        name: "袁术",
        aliases: &["公路"],
    },
    CharacterDef {
        name: "吕布",
        aliases: &["奉先", "吕奉先", "飞将"],
    },
    CharacterDef {
        name: "董卓",
        aliases: &["仲颖"],
    },
    CharacterDef {
        name: "貂蝉",
        aliases: &[],
    },
    CharacterDef {
        name: "蔡琰",
        aliases: &["蔡文姬", "文姬"],
    },
    CharacterDef {
        name: "崔琰",
        aliases: &["崔季珪"],
    },
    CharacterDef {
        name: "蒋琬",
        aliases: &["蒋公琰", "公琰"],
    },
    CharacterDef {
        name: "蔡邕",
        aliases: &["伯喈"],
    },
    CharacterDef {
        name: "董祀",
        aliases: &[],
    },
];

static HONGLOU: &[CharacterDef] = &[
    CharacterDef {
        name: "贾宝玉",
        aliases: &["宝玉", "宝二爷", "怡红公子"],
    },
    CharacterDef {
        name: "林黛玉",
        aliases: &["黛玉", "颦儿", "潇湘妃子", "林妹妹", "林姑娘"],
    },
    CharacterDef {
        name: "薛宝钗",
        aliases: &["宝钗", "宝姐姐", "宝姑娘", "蘅芜君"],
    },
    CharacterDef {
        name: "贾母",
        aliases: &["老太太", "史太君", "老祖宗"],
    },
    CharacterDef {
        name: "王熙凤",
        aliases: &["凤姐", "凤辣子", "琏二奶奶"],
    },
    CharacterDef {
        name: "贾政",
        aliases: &["老爷", "存周"],
    },
    CharacterDef {
        name: "王夫人",
        aliases: &["太太"],
    },
    CharacterDef {
        name: "贾琏",
        aliases: &["琏二爷"],
    },
    CharacterDef {
        name: "李纨",
        aliases: &["宫裁", "稻香老农"],
    },
    CharacterDef {
        name: "贾元春",
        aliases: &["元春", "元妃"],
    },
    CharacterDef {
        name: "贾探春",
        aliases: &["探春", "三姑娘", "蕉下客"],
    },
    CharacterDef {
        name: "贾迎春",
        aliases: &["迎春", "二姑娘"],
    },
    CharacterDef {
        name: "贾惜春",
        aliases: &["惜春", "四姑娘"],
    },
    CharacterDef {
        name: "史湘云",
        aliases: &["湘云", "云妹妹", "枕霞旧友"],
    },
    CharacterDef {
        name: "妙玉",
        aliases: &["槛外人"],
    },
    CharacterDef {
        name: "秦可卿",
        aliases: &["可卿", "蓉大奶奶"],
    },
    CharacterDef {
        name: "薛蟠",
        aliases: &["薛大傻子", "呆霸王"],
    },
    CharacterDef {
        name: "袭人",
        aliases: &["花袭人"],
    },
    CharacterDef {
        name: "晴雯",
        aliases: &[],
    },
    CharacterDef {
        name: "紫鹃",
        aliases: &[],
    },
    CharacterDef {
        name: "平儿",
        aliases: &[],
    },
    CharacterDef {
        name: "香菱",
        aliases: &["甄英莲", "英莲"],
    },
    CharacterDef {
        name: "刘姥姥",
        aliases: &[],
    },
    CharacterDef {
        name: "尤二姐",
        aliases: &[],
    },
    CharacterDef {
        name: "尤三姐",
        aliases: &[],
    },
    CharacterDef {
        name: "柳湘莲",
        aliases: &[],
    },
];

static XIYOU: &[CharacterDef] = &[
    CharacterDef {
        name: "孙悟空",
        aliases: &[
            "悟空",
            "行者",
            "孙行者",
            "齐天大圣",
            "大圣",
            "美猴王",
            "猴王",
            "孙大圣",
            "石猴",
        ],
    },
    CharacterDef {
        name: "唐僧",
        aliases: &["玄奘", "三藏", "唐长老", "御弟", "陈玄奘", "金蝉子"],
    },
    CharacterDef {
        name: "猪八戒",
        aliases: &["八戒", "呆子", "猪悟能", "悟能"],
    },
    CharacterDef {
        name: "沙僧",
        aliases: &["沙和尚", "沙悟净", "悟净"],
    },
    CharacterDef {
        name: "如来佛祖",
        aliases: &["如来", "佛祖", "释迦"],
    },
    CharacterDef {
        name: "观音菩萨",
        aliases: &["观音", "菩萨", "观世音"],
    },
    CharacterDef {
        name: "玉皇大帝",
        aliases: &["玉帝", "玉皇"],
    },
    CharacterDef {
        name: "太上老君",
        aliases: &["老君"],
    },
    CharacterDef {
        name: "菩提祖师",
        aliases: &["菩提"],
    },
    CharacterDef {
        name: "二郎神",
        aliases: &["杨戬"],
    },
    CharacterDef {
        name: "哪吒",
        aliases: &["三太子"],
    },
    CharacterDef {
        name: "铁扇公主",
        aliases: &["铁扇", "罗刹女"],
    },
    CharacterDef {
        name: "牛魔王",
        aliases: &["牛王"],
    },
    CharacterDef {
        name: "红孩儿",
        aliases: &["圣婴大王", "善财童子"],
    },
    CharacterDef {
        name: "白骨精",
        aliases: &["白骨夫人"],
    },
    CharacterDef {
        name: "金角大王",
        aliases: &["金角"],
    },
    CharacterDef {
        name: "银角大王",
        aliases: &["银角"],
    },
    CharacterDef {
        name: "大鹏金翅雕",
        aliases: &["大鹏"],
    },
];

/// List of supported novel names in processing order.
pub const NOVELS: &[&str] = &["水浒传", "三国演义", "红楼梦", "西游记"];

/// Map a novel name to its character definitions.
pub fn get_novel_characters(novel: &str) -> &'static [CharacterDef] {
    match novel {
        "水浒传" => SHUIHU,
        "三国演义" => SANGUO,
        "红楼梦" => HONGLOU,
        "西游记" => XIYOU,
        _ => &[],
    }
}

/// Build a per-novel map from any alias (including the canonical name) to the
/// canonical character name.
///
/// Alias resolution is scoped to a single novel because courtesy names collide
/// across novels — e.g. `公明` is both 宋江 (水浒传) and 徐晃 (三国演义). A
/// global map cannot disambiguate these, so the pipeline resolves aliases
/// within each novel's own map.
pub fn build_alias_map_for_novel(novel: &str) -> HashMap<&'static str, &'static str> {
    let mut map = HashMap::new();
    for cdef in get_novel_characters(novel) {
        map.insert(cdef.name, cdef.name);
        for a in cdef.aliases {
            map.insert(*a, cdef.name);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_novels_have_characters() {
        for novel in NOVELS {
            let chars = get_novel_characters(novel);
            assert!(!chars.is_empty(), "{novel} should have characters");
        }
    }

    #[test]
    fn all_names_are_unique_within_novel() {
        for novel in NOVELS {
            let chars = get_novel_characters(novel);
            let mut names = std::collections::HashSet::new();
            for c in chars {
                assert!(
                    names.insert(c.name),
                    "duplicate name {} in {}",
                    c.name,
                    novel
                );
                for a in c.aliases {
                    assert!(names.insert(a), "duplicate alias {} in {}", a, novel);
                }
            }
        }
    }

    #[test]
    fn alias_map_resolves_all_names() {
        // Alias maps are per-novel: courtesy names like 公明 belong to both
        // 宋江 (水浒传) and 徐晃 (三国演义), so each novel must resolve its own.
        for novel in NOVELS {
            let map = build_alias_map_for_novel(novel);
            for c in get_novel_characters(novel) {
                assert_eq!(
                    map.get(c.name).copied(),
                    Some(c.name),
                    "canonical name {} should map to itself in {novel}",
                    c.name
                );
                for a in c.aliases {
                    assert_eq!(
                        map.get(a).copied(),
                        Some(c.name),
                        "alias {a} should map to {} in {novel}",
                        c.name
                    );
                }
            }
        }
    }
}
