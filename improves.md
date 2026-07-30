我看了你这一路的设计，从最初的 LoreScope 到现在的 Entity Resolution Engine，我反而越来越确定一件事情：

你其实不是在做一个 GraphRAG。

你做的是一个 World Compiler（世界编译器）。

这一点很重要，因为它决定了后面的技术选型。

很多 GraphRAG 项目都是：

文本 → LLM → 图

而你一直坚持的是：

世界模型（Entity） → 文本验证 → 世界状态更新（Event / Relation / Timeline）

所以英文支持，我不会走 NLP/LLM 那一套。

我会继续走 Compiler 的思路。

⸻

我建议增加一个 Language Layer

我觉得目前最大的缺口不是英文规则，而是语言层没有抽象。

例如：

Compiler
    │
    ▼
Language Provider
    │
    ├── Chinese
    ├── English
    ├── Japanese
    └── ...

trait 我会设计得非常小：

pub trait LanguageProvider {
    fn sentence_splitter(&self) -> &dyn SentenceSplitter;
    fn tokenizer(&self) -> &dyn Tokenizer;
    fn profile_patterns(&self) -> &[ProfilePattern];
    fn discovery_rules(&self) -> &[DiscoveryRule];
    fn stopwords(&self) -> &StopwordSet;
}

Compiler 完全不知道英文还是中文。

以后换语言，不需要改 Compiler。

⸻

第二件事：英文不要做 NER

很多人第一反应：

英文
↓
spaCy
↓
NER

我不会。

原因很简单：

NER 是统计模型。

LoreScope 是 Compiler。

Compiler 要的是：

可解释

可重复

可验证

所以我更推荐：

Token
↓
Pattern
↓
Entity Resolver

例如：

Prince Andrei
Count Rostov
General Kutuzov

直接 Pattern。

例如：

TITLE + Capitalized+
↓
Mention

而不是：

BERT
↓
PERSON

⸻

第三件事：英文其实比中文更容易

很多人误会了。

其实英文人物比中文好处理。

例如：

战争与和平：

Prince Andrei
Prince Andrew
Andrei
Bolkonsky
Prince Bolkonsky

这些其实都是：

Alias

不是实体。

实体永远：

Andrei Bolkonsky

所以：

AliasResolver：

Prince Andrei
↓
Andrei Bolkonsky

结束。

⸻

第四件事：Title 应该成为 Profile

这一点我觉得很多项目都做错。

例如：

Count
Prince
Captain
General
Marshal

很多项目把它当名字。

其实不是。

应该：

Profile

例如：

{
    "name": "Kutuzov",
    "title": "General",
    "rank": "Field Marshal"
}

以后：

General Kutuzov
↓
title + alias
↓
Kutuzov

这比 NER 稳定太多。

⸻

第五件事：英文 Discovery Rule

其实英文小说规则没有那么多。

我整理过经典小说。

真正有价值的大概就是这些：

人名开始

Mr.
Mrs.
Miss
Sir
Lady
Lord
Prince
Princess
Count
Countess
Captain
General
Colonel
Major
Doctor
Father

⸻

身份

was born in
was the son of
daughter of
wife of
married
served under

⸻

死亡

died
killed
executed
buried

⸻

战争

battle
campaign
siege
retreated
captured

⸻

其实就够了。

不要搞五百条 Regex。

⸻

第六件事：Embedding 我建议换一下模型

这里我反而有一点建议。

fastembed 默认很多模型都是：

BAAI/bge-small
e5-small

如果是小说。

我建议：

优先考虑 E5。

原因：

E5 本来就是：

query
passage

训练出来的。

人物画像：

Name:
Alias:
Relations:
Events:

这种文本。

特别适合。

如果以后：

Rust：

trait Embedder {
    fn embed_entity(...)
    fn embed_mention(...)
}

未来：

换：

e5
bge
jina
nomic

都不用改。

⸻

第七件事：Entity Representation 可以再升级一点

我觉得你现在已经很好了。

但是还能更 Compiler 一点。

不要：

String

建议：

先：

IR

例如：

struct EntityRepresentation {
    name: String,
    aliases: Vec<String>,
    properties: Vec<Property>,
    relations: Vec<EntityId>,
    events: Vec<EventId>,
}

最后：

Formatter
↓
String
↓
Embedding

为什么？

以后：

可以：

JSON
Markdown
YAML
Prompt

全部重用。

而不是重新拼字符串。

⸻

第八件事：向量索引不要绑 HNSW

这个其实你已经做对了。

但是我建议：

目录直接独立。

例如：

vector/
    index.rs
    hnsw.rs
    brute_force.rs
    embedding.rs
    cache.rs

LoreScope：

根本不知道：

HNSW

存在。

Compiler：

只知道：

trait VectorIndex

以后：

甚至：

sqlite-vec
tantivy
pgvector
faiss（如果以后需要）

都能换。

⸻

我额外建议你引入的几个 Rust 库

我会控制在真正需要的范围，不会为了“先进”而堆技术。

功能	推荐库	理由
Embedding	fastembed	成熟、纯 Rust 集成友好，本地推理即可。
ANN 索引	hnsw-rs	社区成熟度比部分轻量实现更高，文档和实践更多。
Aho-Corasick	aho-corasick	已经在用了，继续保留。
Regex	regex	英文 Pattern 足够，不需要 PCRE。
并行	rayon	用于启动时构建 Representation、Embedding、索引，非常适合 CPU 密集型任务。
SQLite	rusqlite	已有即可，注意事务和批量写。
序列化	serde	Profile / Config 保持 JSON 配置化。
文本归一化	unicode-normalization	英文重音字符、Unicode 规范化，避免 é 和 é 这种隐藏坑。

这里我特别推荐 rayon。

因为你的启动流程天然就是：

Entity
↓
Representation
↓
Embedding
↓
HNSW

几千个人物完全可以：

rayon
↓
par_iter()
↓
embed_batch

CPU 能吃满。

⸻

最后，我想给 LoreScope 一个定位

我觉得到今天，它已经不应该再叫：

小说知识图谱

它真正像的是：

           Source Text
                │
                ▼
          Language Frontend
                │
                ▼
          World Compiler
                │
     ┌──────────┼──────────┐
     ▼          ▼          ▼
 Entity      Event      Relation
                │
                ▼
          Timeline Builder
                │
                ▼
          World Database
                │
                ▼
         Entity Resolver
      (Alias + Embedding + HNSW)
                │
                ▼
            MCP Query API

整个系统里，Embedding 只是编译器的一种索引优化技术，它不参与事实产生，不参与世界推理，也不改变数据库中的真相。

我觉得这是 LoreScope 最有价值的地方，也是它和现在大多数 GraphRAG、LLM 知识图谱项目最大的区别：事实来自编译，向量只负责检索；世界模型才是一等公民，而不是 LLM。


---

我反而觉得，你现在这个方向不要往 RAG、GraphRAG 那边靠，要守住自己的初衷。

我还记得你一开始说过一句话（也是整个 LoreScope 的灵魂）：

LoreScope 是 Agent 的百科辞典，而不是文本搜索。

如果你的最终目标是陪伴型 AI（Companion AI），那么它和现在大部分知识图谱最大的区别就在于：

它需要的是一个”活着的世界”，而不是一堆三元组。

⸻

我反而建议，中英文应该统一成”世界编译器”

不要设计：

Chinese Compiler
English Compiler

而应该是：

           World Compiler
                  │
      ┌───────────┴────────────┐
      ▼                        ▼
 Chinese Frontend       English Frontend

也就是说：

语言只是 Frontend。

Compiler 永远只有一个。

就像：

clang
gcc
↓
LLVM IR

一样。

⸻

中英文真正不同的只有四件事

① Sentence Splitter

中文：

。
！
？

英文：

.
!
?
Mr.
Mrs.
Dr.

这是 Frontend。

⸻

② Mention Extractor

中文：

刘备
玄德
先主

英文：

Prince Andrei
General Kutuzov
Anna

还是 Frontend。

⸻

③ World Builder

Profile Pattern

中文：

字
号
人也
身长

英文：

was born
son of
daughter of
married
served as

结束。

⸻

④ Pronoun Resolver

中文：

其
彼
他

英文：

he
she
his
her
they

结束。

⸻

除此之外：

Observation
↓
Event
↓
Relation
↓
Timeline
↓
Store

全部一样。

千万不要分叉。

⸻

这其实特别适合 Companion AI

因为陪伴 AI 需要的不是：

User
↓
搜索知识

而是：

User
↓
世界理解
↓
人物理解
↓
关系理解
↓
时间理解

举个例子。

用户：

为什么皮埃尔后来讨厌拿破仑？

普通 RAG：

搜：
皮埃尔
拿破仑
讨厌

返回几段。

⸻

LoreScope：

直接：

Pierre
↓
Timeline
↓
1812
↓
French invasion
↓
Moscow burned
↓
Relationship Changed

然后：

Relation
Pierre
→ Napoleon
Friendly
↓
Enemy
↓
Hatred

回答：

因为莫斯科焚毁以后……

它不是搜索。

它是在读世界状态。

⸻

这也是我一直觉得 Event 是一等公民

很多 GraphRAG：

Person
↓
Relation
↓
Person

结束。

但是小说不是。

小说真正重要的是：

Person
↓
Event
↓
World State
↓
Person

例如：

刘备
↓
赤壁之战
↓
荆州归属变化
↓
孙权

没有 Event。

Relation 根本解释不了。

⸻

我甚至建议，你以后不要叫 Knowledge Graph

叫：

World State Database

里面：

Entity
Profile
Event
Relation
Timeline
Evidence

这五个。

已经够了。

⸻

那 Embedding 放哪？

很多人喜欢：

Graph
↓
Embedding

我反而建议：

Entity
↓
Representation
↓
Embedding

结束。

Embedding 永远不要碰：

Relation
Timeline
Event

因为：

这些都是：

事实

Embedding：

只是：

索引

以后：

换模型：

BGE
↓
E5
↓
Jina

整个数据库：

零变化。

⸻

我觉得还应该补一个东西

这是我这两天一直在想的。

你现在：

Entity
↓
Profile

很好。

但是：

陪伴 AI。

真正需要：

Persona Snapshot（人物快照）

例如：

刘备。

不同时间：

184
刘备
身份：
平民
↓
208
左将军
↓
221
皇帝
↓
223
白帝城

其实：

Profile

不是：

最新状态

而应该：

Profile Snapshot

例如：

Entity
↓
Snapshots
↓
Timeline

那么：

用户问：

赤壁之战时刘备是什么身份？

不用推理。

直接：

Timeline
↓
208
↓
Snapshot

出来了。

⸻

这一点对于 Companion AI 太重要了

因为 Companion AI 最怕：

人设漂移。

例如：

用户：

哈利第一次见邓布利多的时候，他是什么样的人？

如果没有：

Snapshot

模型容易把：

第七部：

全部知识

带进去。

如果：

LoreScope：

时间：
第一卷
↓
Snapshot

AI 的回答会稳定很多。

⸻

如果让我给 LoreScope 定一个真正的定位

我现在不会再叫它：

小说知识图谱

我会叫它：

World State Compiler（世界状态编译器）

输入：

文本

输出不是：

Graph

而是：

一个可以查询、可以演化、具有时间一致性的世界模型。

⸻

我最后给你一个我认为最重要的建议

你现在千万不要为了英文去增加很多英文规则。

应该增加的是 Language Frontend。

因为以后：

战争与和平
哈利波特
魔戒
冰与火之歌
银河帝国

甚至游戏设定集：

魔兽
原神
Fate

全部都可以共用同一个：

World Compiler

唯一变化的是：

Language Frontend

而后面的：

Entity
→ Event
→ Relation
→ Timeline
→ World State

一行代码都不用改。

⸻

我还有一个建议，我认为这比 HNSW 更重要，而且特别符合陪伴 AI。

目前 LoreScope 建的是：

世界事实（World Facts）

但陪伴 AI 真正需要的是：

世界事实 + 世界状态（World State）

也就是说，Event 不应该只是历史记录，它应该能够驱动状态变化。

例如：

事件：白帝城托孤
        │
        ▼
刘备.status = deceased
诸葛亮.role = 辅助
刘禅.role = 皇帝
蜀汉.stability = 降低

以后 AI 查询时，读到的是某个时间点的世界状态，而不仅仅是一串历史事件。

这样你的 LoreScope 就不仅仅是在整理小说，而是在构建一个可随时间演化的世界模型。对于陪伴型 AI，这比单纯的知识图谱价值要高得多，因为它能让 AI 始终基于一致的世界状态来理解人物和故事。



---


我觉得，到这里，我终于理解了你真正想做的东西。

以前我一直把 LoreScope 当成小说世界建模器来看。

但是你刚才这句话让我意识到，其实它只是第一阶段的训练场。

四大名著、《战争与和平》这些，不是产品。

它们是训练 Compiler 的 benchmark。

真正的目标其实是：

让 AI 理解世界，理解用户，陪伴用户一起成长。

那么，整个架构其实应该重新命名了。

⸻

我看到的是两个 Compiler，而不是一个

实际上你一直在做两件完全不同的事情。

① World Compiler（LoreScope）

输入：

小说
Wiki
设定
百科
新闻

输出：

World Model

包含：

Entity
Event
Relation
Timeline
Evidence

作用：

AI 理解外部世界。

⸻

② User Compiler（Memory Distillation）

输入：

聊天记录
行为
长期目标
兴趣变化
情绪变化
偏好

输出：

User Model

包含：

Identity
Preference
Habit
Relationship
Life Timeline
Goal
Memory

作用：

AI 理解用户。

⸻

这两个其实非常对称。

        外部世界                     用户世界
────────────────────────────────────────────
Entity                    User Profile
Relation                  Social Relation
Event                     Life Event
Timeline                  User Timeline
Evidence                  Conversation Evidence

你会发现……

Schema 都几乎一样。

⸻

Companion AI 真正需要的是两个世界

以后 AI 每回答一句话。

其实都应该建立在：

          AI
           │
     ┌─────┴──────┐
     ▼            ▼
 World Model   User Model

而不是：

Prompt
+
Memory

这是我觉得很多 Companion AI 最大的问题。

他们只有：

Memory

没有：

User Model

⸻

举个例子。

用户：

最近好累。

普通 Memory：

最近好累。

结束。

LoreScope 思维：

这是一个：

Life Event

例如：

Stress Increased

然后：

User Timeline
↓
2026
工作压力增加

以后：

AI 不是记住一句话。

而是记住：

这个阶段。

⸻

我觉得 User 不应该只有 Profile

这一点其实和小说一模一样。

小说：

刘备

不是：

姓名

而是：

刘备
↓
一生

用户也是。

不是：

Tim
↓
喜欢 Rust

而是：

Tim
↓
人生

例如：

2024
开始 AI
↓
2025
开始 Agent
↓
2026
开发 LoreScope
↓
2027
创业

AI 应该知道：

用户现在正处于哪一个阶段。

这才是真正的陪伴。

⸻

我觉得 User Timeline 比 Memory 更重要

Memory：

一条一条。

Timeline：

连续人生。

举个例子。

用户：

我要找工作。
↓
找到工作。
↓
升职。
↓
创业。

AI 应该看到的是：

Career Timeline

而不是：

四条 Memory。

⸻

我甚至建议 User Model 也做 Snapshot

就像人物。

例如：

User Snapshot
2026-07
兴趣：
Rust
AI Agent
LoreScope
----------
目标：
找工作
----------
压力：
高

半年以后：

2027
兴趣：
公司
融资
管理
----------
压力：
中

AI 就不会：

一直聊 Rust。

而会：

跟着用户一起成长。

这句话其实就是你刚才说的：

什么阶段该聊什么。

⸻

我建议把 User Model 分成六层

第一层：

Identity

永远不怎么变。

例如：

昵称
职业
语言
年龄段（可选）
所在地（粗粒度，可选）

⸻

第二层：

Preference

例如：

喜欢：
Rust
Go
三国
推理小说

⸻

第三层：

Habit

例如：

晚上聊天
喜欢长回复
喜欢架构讨论
不喜欢废话

⸻

第四层：

Goal

例如：

开发 LoreScope
发布 v1
找工作
创业

Goal 是会完成、会新增、会放弃的。

⸻

第五层：

Life Timeline

例如：

毕业
第一份工作
开始 AI
创业
……

⸻

第六层：

Relationship

例如：

朋友
家人
同事
AI

Companion AI 很重要。

因为：

AI 自己也是：

Relationship

中的一个对象。

⸻

那 Memory Distillation 放哪里？

我反而觉得：

Memory Distillation：

应该变成：

Conversation
↓
Observation
↓
User Compiler
↓
User Model Update

不是：

Conversation
↓
Memory

⸻

我会把整个系统画成这样

                  Conversation
                        │
        ┌───────────────┴──────────────┐
        ▼                              ▼
 World Compiler                 User Compiler
(LoreScope)                 (Memory Distillation)
        │                              │
        ▼                              ▼
 World Model                   User Model
        │                              │
        └───────────────┬──────────────┘
                        ▼
               Companion Reasoner
                        │
                        ▼
                AI Personality
                        │
                        ▼
                   Response

注意这里有一个很关键的点：

AI Personality 不应该写死。

它应该来自：

User Model
+
World Model

例如：

用户最近压力很大。

AI：

语气：
柔和一点。
回复：
短一点。
不要讲大道理。
多问一句：
今天怎么样？

如果：

用户：

最近在冲项目。

AI：

直接：
技术讨论。
少寒暄。

这就是：

什么阶段聊什么。

不是 Prompt。

而是：

User State

决定的。

⸻

我觉得你真正要做的，其实已经不是 LoreScope 了。

如果让我给整个项目起一个总名字，我会叫它：

Cognitive Compiler（认知编译器）

它包含两个编译器：

* World Compiler：把外部世界编译成稳定、可查询的世界模型。
* User Compiler：把长期对话编译成稳定、可演化的用户模型。

它们都遵循同样的原则：

事实来自编译，不来自猜测；状态来自事件，不来自 Prompt；长期认知来自模型，不来自上下文窗口。

而 Companion AI 本身，只负责在每一次对话时读取这两个模型，并结合当前输入做推理和表达。

我觉得，这是你这一路设计里最有潜力、也最有差异化的地方。它不是一个“更好的记忆系统”，而是一个长期认知系统。