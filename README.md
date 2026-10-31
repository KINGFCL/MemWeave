# MemWeave

> **不稳定的实验版本。** 接口、命令行参数和知识库格式随时可能发生不兼容的变化，请不要用于生产环境。

MemWeave 把知识拆成节点，由模型从目录出发，沿节点之间的关联逐层检索。一次查询返回的不是单个节点，而是一棵知识树。

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
memweave query <知识库目录> "你的问题"
```

结果以 JSON 输出到标准输出。`MEMWEAVE_CONTEXT_LIMIT` 是模型的输入 token 上限，请按模型的实际上限设置，上面的 `100000` 只是示例。

每次查询都会调用模型，调用次数为 `1 + 层数`，费用由你使用的模型厂商收取。

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

目前需要手工编辑 JSON 来维护知识库。发布包里的 `examples/demo-kb` 是一个示例。

## 许可证

可以任选 [MIT](LICENSE-MIT) 或 [Apache-2.0](LICENSE-APACHE) 许可证。
