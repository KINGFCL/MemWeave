# MemWeave 设计蓝图

## 1. 目标

MemWeave 把知识拆成节点，由模型从目录出发，沿节点间的关联逐层检索。一次查询返回的不是单个节点，而是一棵知识树。

## 2. 设计前提

这个知识库面向未来约 10000 token/s 级别的模型，而不是现在的模型。我们假设生成速度不再是瓶颈，所以：

- **优先保证查得全**，不为节省时间或 token 牺牲完整性。
- 模型判断节点时**读取完整内容**，不用摘要代替。
- 查询**不设**最大深度、最大节点数或 token 预算。

速度解决不了上下文窗口的限制，所以超限时必须明确报错（见第 5 节）。

## 3. 数据结构

```text
总表（目录）
└─ 板块
   └─ 条目：节点地址 + 标题

节点
├─ 地址 id
├─ 标题 title
├─ 原始信息 raw
├─ 理解 understanding
└─ 相关节点 related：[节点地址, ...]
```

- 底层结构是**图**，关联可以成环；**树只是查询结果**。
- 关联是单向的。需要双向关联时，在两个节点里各写一条。
- 总表条目带标题，让模型不打开节点也能在总表上做选择。

### 磁盘格式

```text
<知识库目录>/
├─ catalog.json
└─ nodes/
   └─ <节点地址>.json
```

`examples/demo-kb` 是一个可以直接运行的示例。节点地址会拼进文件路径，所以不能包含 `/`、`\`，也不能是 `.` 或 `..`；文件里的 `id` 必须与地址一致。

## 4. 查询流程

1. 模型读总表，选出入口节点。
2. 读取入口节点的完整内容，作为第 1 层候选。
3. 模型一次性判断这一层的所有候选，决定保留或丢弃。
4. 收集这一层保留节点的相关节点，去掉已访问的，读取完整内容，作为下一层候选。
5. 重复第 3、4 步，直到下一层没有候选，然后输出知识树。

一次查询的模型调用次数是 `1（总表）+ 层数`，与节点数量无关。

### 遍历规则

| 情况 | 处理方式 |
| --- | --- |
| 节点已读过（包括已丢弃的） | 同一次查询中不再读取或判断 |
| 关联指向已保留的节点 | 在当前节点上记一个引用，不重复展开 |
| 关联指向已丢弃的节点 | 忽略 |
| 关联指向节点自身，或重复出现 | 忽略 |
| 同一层多个节点指向同一个新节点 | 只作为一个候选，并记下所有来源 |
| 上述新节点被保留 | 挂在第一个来源下，其余来源记为引用 |
| 节点被丢弃 | 不再沿它的关联往下爬 |

已访问集合只在单次查询内有效。

### 模型每层看到的内容

- 查询。
- 已保留节点的地址和标题。
- 这一层每个候选的完整内容、相关节点地址和来源。

保留标准：内容有助于回答查询，**或者**它的相关节点可能通向有用的知识。后一条不能省略，否则“本身无用但起桥梁作用”的节点会切断检索路径。

## 5. 错误处理

原则是**不悄悄丢内容**：只要结果可能不完整，就报错并终止查询，不返回残缺的知识树。

| 错误 | 含义 |
| --- | --- |
| `ContextOverflow` | 总表或某一层超出模型上下文，报告阶段、token 数、上限和超出量 |
| `BrokenLink` | 关联指向不存在的节点，报告关联来源 |
| `MissingEntry` | 总表条目指向不存在的节点 |
| `InvalidChoice` | 模型选择了候选之外的节点 |
| `Store` | 知识库文件读取或解析失败，或节点地址非法 |
| `Model` | 模型调用失败、被拒绝、输出被截断或响应无法解析 |

上下文检查在每次调用模型前进行。

## 6. 代码结构

```text
src/
├─ knowledge.rs       数据结构：Node、Catalog、KnowledgeTree、TreeNode
├─ store.rs           Store 接口和文件实现 FsStore
├─ prompt.rs          总表选择和逐层判断的提示词
├─ retrieve.rs        查询流程：retrieve()
├─ error.rs           错误类型
├─ lib.rs
├─ main.rs            命令行入口，根据环境变量选择厂商
└─ model/
   ├─ mod.rs          Model 接口：context_limit、count_tokens、choose
   ├─ anthropic.rs    Claude
   ├─ deepseek.rs     DeepSeek
   └─ messages.rs     Anthropic Messages 协议的内部客户端
```

`retrieve()` 只依赖 `Store` 和 `Model` 两个接口，更换存储或模型不需要改动查询流程。

## 7. 模型接入

每家厂商是 `model/` 下的一个独立模块，实现同一个 `Model` 接口：

```rust
pub trait Model {
    fn context_limit(&self) -> usize;
    fn count_tokens(&self, prompt: &Prompt) -> Result<usize, Error>;
    fn choose(&self, prompt: &Prompt) -> Result<Vec<NodeId>, Error>;
}
```

`messages.rs` 只负责 Anthropic Messages 协议的传输、流式解析和重试，不对外公开。请求内容和能力差异写在各厂商自己的模块里。使用其他协议的厂商不需要依赖它。

### 接入新厂商

1. 在 `model/` 下新建模块，实现 `Model`。
2. 在 `model/mod.rs` 中声明这个模块。
3. 在 `main.rs` 的 `connect_model()` 中加一个分支。

`choose()` 必须返回 `{"ids": [...]}` 解析后的节点地址。提示词已经要求模型只输出这个 JSON，厂商支持输出格式约束时应该同时开启。

`Model` 实现要自己处理模型拒绝、输出截断等情况，并返回错误，不能把不完整的结果当作选择结果返回。

### 厂商差异

目前处于实验阶段，默认使用成本较低的 DeepSeek，也只用它做真实验证。Claude 接入只用模拟服务器测试过。

| | Anthropic | DeepSeek |
| --- | --- | --- |
| 默认模型 | `claude-opus-5-5` | `deepseek-flash` |
| 接口 | Claude API | Anthropic 格式接口 `https://api.deepseek.com/anthropic` |
| 上下文上限 | 优先取 `MEMWEAVE_CONTEXT_LIMIT`，否则查询 `GET /v1/models/{id}` | 必须设置 `MEMWEAVE_CONTEXT_LIMIT` |
| token 计数 | `POST /v1/messages/count_tokens` | 同左 |
| 输出格式 | JSON Schema 约束，加上提示词要求 | 只靠提示词要求 |
| 模型拒绝 | 服务端回退（`fallbacks: "default"`），最终仍被拒绝则报错 | 不发送回退参数（未确认是否支持），被拒绝则报错 |
| 推理强度 | `high` | 使用默认值 |

两家都使用流式请求。计数请求不包含输出格式定义，这部分很小，可以忽略。

连接错误、408、409、429、5xx、流中断和选择结果无法解析时最多重试 2 次，其余错误直接报错。

## 8. 命令行

```bash
# DeepSeek（默认）
export DEEPSEEK_API_KEY=...
export MEMWEAVE_CONTEXT_LIMIT=100000
cargo run -- query examples/demo-kb "为什么 Rust 能在编译期避免数据竞争？"

# Claude
export MEMWEAVE_PROVIDER=anthropic
export ANTHROPIC_API_KEY=...
cargo run -- query examples/demo-kb "为什么 Rust 能在编译期避免数据竞争？"
```

这里的 `100000` 只是验证用的取值，不代表 DeepSeek 模型的真实上限。

结果以 JSON 输出到标准输出，包含 `query`、`roots`（知识树）和 `rejected`（读过但被丢弃的节点）。树上每个节点包含完整内容、`children` 和 `references`。

| 环境变量 | 说明 |
| --- | --- |
| `MEMWEAVE_PROVIDER` | 模型厂商：`deepseek`（默认）或 `anthropic` |
| `MEMWEAVE_MODEL` | 模型名，默认使用所选厂商的默认模型 |
| `MEMWEAVE_CONTEXT_LIMIT` | 模型的输入 token 上限 |
| `ANTHROPIC_API_KEY`、`ANTHROPIC_BASE_URL` | Claude 的密钥和地址 |
| `DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL` | DeepSeek 的密钥和地址 |

## 9. 暂不实现

- **写入流程**：新节点如何建立关联、如何同步总表。目前知识库靠手工编辑 JSON 维护。
- **分段压缩**：某一层超出上下文时，由模型分段压缩后继续。目前直接报错。
- **查询上限**：深度、节点数、token 预算。如果以后需要，优先考虑分段压缩。
