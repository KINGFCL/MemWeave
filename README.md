# MemWeave

> **不稳定的实验版本。** 接口、命令行参数和知识库格式随时可能发生不兼容的变化，请不要用于生产环境。

MemWeave 把知识拆成节点，由模型从目录出发，沿节点之间的关联逐层检索。一次查询返回的不是单个节点，而是一棵知识树。

知识库可以手工维护，也可以由 Weaver（织者）调用模型从文本生成。

## 工作方式

1. 模型读总表，选出入口节点。
2. 读取这些节点的完整内容，模型一次性判断这一层的所有候选，决定保留或丢弃。
3. 收集保留节点的相关节点，去掉已访问的，作为下一层候选。
4. 重复第 2、3 步，直到没有新候选，然后输出知识树。

查询优先保证查得全，不限制深度和节点数。如果某一层超出模型的上下文上限，查询会直接报错，不会悄悄截断。

## 安装

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
```

Weaver 目前只支持文本文件，每次生成一个全新的知识库。节点的原文由程序从输入中按行提取，不会被模型改写；每一行都必须归入至少一个节点。文本超出单次模型调用的能力时会报错，不会截断。

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
├─ catalog.json        总表：板块和节点地址
└─ nodes/
   └─ <节点地址>.json   节点：标题、原始信息、理解、相关节点
```

每次查询前都会把整个知识库解析进内存并校验，重复节点、失效关联、从总表到达不了的节点都会一次列出并报错，这时不会调用模型。总表只需要按领域列出几个代表节点，其他节点沿关联到达。

发布包里的 `examples/demo-kb` 是一个示例。

## 许可证

可以任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可证。
