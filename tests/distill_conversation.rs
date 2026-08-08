//! Memory Distillation on our dev conversation.
//! Run: cargo test --test distill_conversation distill_dev_session -- --nocapture

use mnemosyne::conversation_compiler::ConversationCompiler;
use mnemosyne::types::Message;

#[test]
fn distill_dev_session() {
    let compiler = ConversationCompiler::new();

    // Structure messages so the extractor can detect problem→solution patterns
    // and decision patterns
    let msgs = vec![
        // ── Phase 1: Architecture Diagnosis ────────────────────────
        Message::new("user", "看看当前代码库到底有多乱，如何理清项目架构？"),
        Message::new(
            "assistant",
            "项目有三个互不兼容的知识模型并存：src/knowledge/mod.rs的通用模型、src/knowledge_object.rs的UUID模型、src/character.rs的V1领域模型。此外编译断在E0583，两条产品线被硬塞在同一个crate里。解决方案：删除knowledge_object.rs，重建knowledge_tools.rs修复编译，分离两条Pipeline。",
        ),
        Message::new(
            "user",
            "两条业务线并存，不冲突。一个通用的，一个是特定场景的。",
        ),
        Message::new(
            "assistant",
            "Memory Distillation是agent回忆录，LoreScope是百科辞典。共享Store API，不共享Pipeline。决定：Memory Distillation不动，LoreScope新建compiler/目录。",
        ),
        // ── Phase 2: Architecture Evolution (V1→V7) ──────────────
        Message::new(
            "user",
            "如何设计Compiler Pipeline？从V1到V5迭代了5个版本，怎么收敛？",
        ),
        Message::new(
            "assistant",
            "V1：先诊断现状→V2：增加Fact/Validator层被否决→V3：开始删东西收敛→V4：固定边界→V5：冻结。关键教训：过早设计Fact层和跨句Observation都是过度设计。核心原则：目标不是文本理解，是世界建模。",
        ),
        Message::new(
            "user",
            "目标不是文本理解，是世界建模。人物节点不是动态产生的垃圾节点。",
        ),
        Message::new(
            "assistant",
            "V7架构：两Pass编译器。Pass 1 World Builder从简介创建实体节点，Pass 2 Story Compiler挂载事件。Entity-centric，不是Observation-centric。决定：推倒V5从头来，采用Entity-centric世界模型。",
        ),
        // ── Phase 3: Config Migration ─────────────────────────────
        Message::new(
            "user",
            "hardcode的代码如何改进？所有的配置东西，全部打进config里。",
        ),
        Message::new(
            "assistant",
            "创建config/entity_profiles/目录，实现JsonEntityProvider。四大名著角色数据从ingest/characters.rs的Rust静态数组移到JSON文件。决定：用JsonEntityProvider代替硬编码的NovelProvider。",
        ),
        // ── Phase 4: Auto-discovery ─────────────────────────────
        Message::new("user", "直接从corpus/*.txt读角色，不要扒V1数据"),
        Message::new(
            "assistant",
            "Profile Extractor改成接受Option<&EntityDictionary>。有dict时精确匹配，无dict时用启发式自动发现。discover_entity_name函数扫描文本中的\"字/者也/身长/面如/使/姓/号\"等模式标记提取实体名。全本三国演义从文本中发现57个角色。",
        ),
        // ── Phase 5: MCP Tools ──────────────────────────────────
        Message::new("user", "如何用MCP查曹仁的关系网络？"),
        Message::new(
            "assistant",
            "通过V1→migration创建持久化DB，用inspect_entity和relation_graph查询。发现曹仁有1条关联边指向孙策(entity_id=6873)，但原文是\"以曹仁之女许配孙策幼弟孙匡\"，关联对象错了——应该是孙匡不是孙策。",
        ),
        Message::new("user", "修复，增加mcp tool，知道错了能修改db"),
        Message::new(
            "assistant",
            "创建孙匡节点(id=3112)，更新edge 6873的target_id。新增correct_relation MCP工具用于校正错误关系。决定：MCP新增correct_relation工具，支持source/predicate/old_target/new_target四参数校正。",
        ),
        // ── Phase 6: Documentation ───────────────────────────────
        Message::new(
            "user",
            "更新readme，同步并存 记忆蒸馏，知识图谱，以及这个mcp",
        ),
        Message::new(
            "assistant",
            "README更新为完整文档，涵盖两条产品线、15个MCP工具清单、双Pass编译器架构、核心Schema。写入plan/postmortem.md记录10个坑和关键决策复盘。",
        ),
    ];

    let compiled = compiler.compile("lorescope-dev", &msgs);

    eprintln!("\n========== Memory Distillation 蒸馏结果 ==========\n");

    eprintln!(
        "━━━ 知识 ({} 条) ━━━━━━━━━━━━━━━━━━━━━━━━━━\n",
        compiled.knowledge.len()
    );
    for (i, k) in compiled.knowledge.iter().enumerate() {
        let score = (k.importance * 100.0) as u32;
        let summary = if k.summary.is_empty() {
            &k.content
        } else {
            &k.summary
        };
        let snippet: String = summary.chars().take(140).collect();
        eprintln!("  {:>2}. [{}%] {:?}", i + 1, score, k.memory_type);
        eprintln!("       {}", snippet);
    }

    eprintln!(
        "\n━━━ 决策 ({} 条) ━━━━━━━━━━━━━━━━━━━━━━━━━━\n",
        compiled.decisions.len()
    );
    for (i, d) in compiled.decisions.iter().enumerate() {
        let score = (d.importance * 100.0) as u32;
        eprintln!(
            "  {:>2}. [{}%] {} (module: {})",
            i + 1,
            score,
            d.decision,
            d.module
        );
        if !d.rationale.is_empty() {
            eprintln!(
                "      理由: {}",
                d.rationale.chars().take(140).collect::<String>()
            );
        }
    }

    eprintln!("\n━━━ 会话状态 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    let s = &compiled.session;
    if !s.current_goal.is_empty() {
        eprintln!("  目标: {}", s.current_goal);
    }
    if !s.current_module.is_empty() {
        eprintln!("  模块: {}", s.current_module);
    }
    if !s.open_problems.is_empty() {
        eprintln!("\n  未完成项:");
        for p in &s.open_problems {
            eprintln!("    - {}", p);
        }
    }
}
