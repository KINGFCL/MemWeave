# MemWeave

> **不稳定的实验版本。** 接口、命令行参数和知识库格式随时可能发生不兼容的变化，请不要用于生产环境。

MemWeave 把知识拆成节点，由模型从目录出发，沿节点之间的关联逐层检索。一次查询返回的不是单个节点，而是一棵知识树。

知识库可以手工维护，也可以由 Weaver（织者）调用模型从文本生成。

节点只保存模型自己的描述和证据，原始数据留在固态硬盘上，需要从底层确认时才按证据读取。记忆可以改，原始数据不可变，模型发现记忆和现实对不上时，可以追溯这条结论是怎么来的。

## 工作方式

1. 模型读总表，选出入口节点。
2. 读取这些节点的描述，模型一次性判断这一层的所有候选，决定保留或丢弃。
3. 收集保留节点的相关节点，去掉已访问的，作为下一层候选。
4. 重复第 2、3 步，直到没有新候选，然后输出知识树。

查询优先保证查得全，不限制深度和节点数。如果某一层超出模型的上下文上限，查询会直接报错，不会悄悄截断。

## 安装

记忆层用 C++ 实现，编译需要支持 C++20 的编译器。

```bash
cargo install memweave --version 0.1.0-alpha.1
```

## 使用

```bash
export DEEPSEEK_API_KEY=...
export MEMWEAVE_CONTEXT_LIMIT=100000

# 调用模型把文本组织成新的知识库。目标目录必须不存在或为空，不会覆盖已有内容。
memweave weave notes.txt my-kb

# 从知识库检索
memweave query my-kb "你的问题"

# 读取节点的证据原文，不调用模型
memweave evidence my-kb <节点地址>

# 用记忆指令直接操作记忆，默认只读，不调用模型
memweave run my-kb "show ownership"
memweave run --write my-kb "link a -> b"

# 输出记忆指令的说明，放进模型的提示词
memweave spec
```

记忆指令写错时会像编译器一样报错，指出位置和改法，这时记忆完全不变。修改会记入知识库目录的 `journal.jsonl`，下次加载时自动恢复。

Weaver 目前只支持文本文件，每次生成一个全新的知识库。输入会原样保存为来源，证据的位置由程序算出，模型无法改写；每一行都必须归入至少一个节点。文本超出单次模型调用的能力时会报错，不会截断。

查询结果以 JSON 输出到标准输出。`MEMWEAVE_CONTEXT_LIMIT` 是模型的输入 token 上限，请按模型的实际上限设置，上面的 `100000` 只是示例。

每次查询都会调用模型，调用次数为 `1 + 层数`；Weaver 每个文件调用一次，输出有问题时最多再重试几次。费用由你使用的模型厂商收取。

| 环境变量 | 说明 |
| --- | --- |
| `MEMWEAVE_PROVIDER` | 模型厂商：`deepseek`（默认）或 `anthropic` |
| `MEMWEAVE_MODEL` | 模型名，默认使用所选厂商的默认模型 |
| `MEMWEAVE_CONTEXT_LIMIT` | 模型的输入 token 上限，使用 DeepSeek 时必填 |
| `DEEPSEEK_API_KEY`、`DEEPSEEK_BASE_URL` | DeepSeek 的密钥和地址 |
| `ANTHROPIC_API_KEY`、`ANTHROPIC_BASE_URL` | Claude 的密钥和地址 |

目前只用真实的 DeepSeek 模型验证过，Claude 接入只用模拟服务器测试过。

## 知识库格式

```text
<知识库目录>/
├─ catalog.json        总表：板块的名字、描述和代表节点的地址
├─ nodes/
│  └─ <节点地址>.json   节点：标题、描述、证据、相关节点
└─ sources/
   └─ <内容哈希>        来源：不可变的原始数据
```

每次查询前都会把整个知识库解析进内存并校验，重复节点、失效关联、从总表到达不了的节点、没有证据或证据无效的节点都会一次列出并报错，这时不会调用模型。总表只需要按领域列出几个代表节点，其他节点沿关联到达。

发布包里的 `examples/demo-kb` 是一个示例。

## 许可证

可以任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可证。
