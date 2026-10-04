//! Relation typing against verbatim corpus prose.
//!
//! These pin the mistakes the real corpus run exposed: a form of address, or a
//! bystander who merely appears in the same sentence, used to type the wrong
//! pair. Every excerpt is copied verbatim from `corpus/` — the novels the V1
//! pipeline actually ingests — so the test fails if the typing rules drift away
//! from what the text supports.
//!
//! ## A known limit, deliberately not asserted
//!
//! `临行，令玄德仍屯兵小沛，与吕布结为兄弟` (三国演义) states a genuine bond
//! between 刘备 and 吕布 and is **not** typed: the pair and the phrase span 45
//! bytes, while `宋江主张一丈青与王英配为夫妇` spans 42 and must stay generic
//! because 宋江 is not one of that marriage's arguments. A distance window cannot
//! separate 42 from 45, so the two cases need argument-structure analysis that
//! this heuristic does not have. Pinning either behaviour here would freeze an
//! accident of the current window rather than a property of the text.

use mnemosyne::ingest::relation::detect_relation_type;

/// 三国演义, 玄德 introduces his sworn brothers to 公孙瓒.
const SANGUO_OATH: &str = "玄德曰：“此关羽、张飞，备结义兄弟也。”";

/// 水浒传, 宋江 marries 一丈青 (扈三娘) to 王英.
const SHUIHU_MARRIAGE: &str = "话说宋江主张一丈青与王英配为夫妇，众人都称赞";

/// 红楼梦, a social call: 邢夫人 and 王夫人 appear only in the narration.
const HONGLOU_CALL: &str = "氏乃治酒，请贾母，邢夫人，王夫人等赏花．是日先携了贾蓉之妻";

/// Objective: Verify a bond the text states is typed for the pair it states it
/// about.
/// Invariants: the sentence naming `结义兄弟` types 关羽/张飞 as 结义.
#[test]
fn attested_bond_is_typed() {
    assert_eq!(
        detect_relation_type(SANGUO_OATH, "关羽", "张飞"),
        "结义",
        "the sworn brothers the text names must be typed"
    );
}

/// Objective: Verify the two arguments a marriage names are typed, and that a
/// bystander in the same sentence is not.
/// Invariants: 扈三娘/王英 are 夫妻; 宋江 — who only arranges it — stays generic.
#[test]
fn marriage_types_the_couple_not_the_bystander() {
    assert_eq!(
        detect_relation_type(SHUIHU_MARRIAGE, "扈三娘", "王英"),
        "夫妻",
        "the couple named by `配为夫妇` must be typed"
    );
    assert_eq!(
        detect_relation_type(SHUIHU_MARRIAGE, "宋江", "王英"),
        "关联",
        "the character who arranged the marriage is not one of its arguments"
    );
}

/// Objective: Verify narration that merely mentions a form of address types
/// nothing — the word has to be used *to address* someone.
/// Invariants: a social call that lists two 夫人 stays generic.
#[test]
fn narration_mentioning_an_address_form_types_nothing() {
    assert_eq!(
        detect_relation_type(HONGLOU_CALL, "贾母", "王夫人"),
        "关联",
        "a social call is not a marriage"
    );
}
