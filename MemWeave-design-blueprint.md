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
├─ knowledge.rs  数据结构：Node、Catalog、KnowledgeTree、TreeNode
├─ store.rs      Store 接口和文件实现 FsStore
├─ model.rs      Model 接口：context_limit、count_tokens、choose
├─ prompt.rs     总表选择和逐层判断的提示词
├─ retrieve.rs   查询流程：retrieve()
├─ anthropic.rs  Model 的 Claude API 实现
├─ error.rs      错误类型
├─ lib.rs
└─ main.rs       命令行入口
```

`retrieve()` 只依赖 `Store` 和 `Model` 两个接口，更换存储或模型不需要改动查询流程。

## 7. 模型接入

默认使用 Claude API 和 `claude-opus-5-5`，可以用环境变量更换模型或改用 Anthropic 兼容服务：

- 上下文上限：优先取 `MEMWEAVE_CONTEXT_LIMIT`，否则启动时从 `GET /v1/models/{id}` 的 `max_input_tokens` 读取。
- token 计数：`POST /v1/messages/count_tokens`。计数请求不包含输出格式定义，这部分很小，可以忽略。
- 选择结果：`POST /v1/messages`，流式读取，用 JSON Schema 约束输出为 `{"ids": [...]}`，推理强度为 `high`。部分兼容服务会忽略这个约束，所以提示词里也要求只输出 JSON。
- 拒绝处理：开启服务端回退（`fallbacks: "default"`），模型拒绝时由 API 换模型重试；最终仍被拒绝则报错。
- 重试：连接错误、408、409、429、5xx、流中断和选择结果无法解析时最多重试 2 次。其余错误直接报错。

### 用 DeepSeek 验证

DeepSeek 的 Anthropic 兼容地址不提供 Models API，需要指定上下文上限：

```bash
export ANTHROPIC_API_KEY=...
export ANTHROPIC_BASE_URL=https://api.deepseek.com/anthropic
export MEMWEAVE_MODEL=deepseek-flash
export MEMWEAVE_CONTEXT_LIMIT=100000
cargo run -- query examples/demo-kb "为什么 Rust 能在编译期避免数据竞争？"
```

这里的 `100000` 只是验证用的取值，不代表模型的真实上限。

它的计数接口可用，但会忽略 `output_config.format` 和 `fallbacks`，也就是没有输出格式保证和服务端回退。

## 8. 命令行

```bash
export ANTHROPIC_API_KEY=...
cargo run -- query examples/demo-kb "为什么 Rust 能在编译期避免数据竞争？"
```

结果以 JSON 输出到标准输出，包含 `query`、`roots`（知识树）和 `rejected`（读过但被丢弃的节点）。树上每个节点包含完整内容、`children` 和 `references`。

可选环境变量：`ANTHROPIC_BASE_URL`、`MEMWEAVE_MODEL`、`MEMWEAVE_CONTEXT_LIMIT`。

## 9. 暂不实现

- **写入流程**：新节点如何建立关联、如何同步总表。目前知识库靠手工编辑 JSON 维护。
- **分段压缩**：某一层超出上下文时，由模型分段压缩后继续。目前直接报错。
- **查询上限**：深度、节点数、token 预算。如果以后需要，优先考虑分段压缩。
