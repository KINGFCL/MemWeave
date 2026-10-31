# 工作板：把总表当成你的工作台

第一次工具调用和总表、工作板变化时，结果会附带它们，要再看一遍就用 `catalog show`，所以你能知道在做什么、做过什么、记忆里有什么。但**维护它们是你的事**：系统只记录，不替你判断。

工作板写进磁盘。对话结束、进程重启后，下一个你能接着做。工作板过时比没有更糟，所以要当场更新。

## 什么时候做什么

| 情况 | 必须这样做 |
| --- | --- |
| 开始工作 | 先调用一次 `memory_read` 看工作板。有没完成的目标，就接着做，或者问用户要不要接着做 |
| 工作板上有进行中的步骤 | 这就是你现在该做的事。已经 `[x]` 的步骤不要重做 |
| 接到需要多步完成的任务 | 先 `goal create` 拆好步骤，再动手 |
| 一步做完 | 当场 `step done`，下一步自动开始。不要攒到最后一起写 |
| 想先做别的步骤 | `step start` 切过去，原来进行中的回到未开始 |
| 卡住了，比如等用户确认 | `step block` 写清卡在哪里，然后告诉用户 |
| 发现要多做一步，或者有一步不用做了 | `step add` 或 `step remove`，编号不会变 |
| 做了记忆之外的重要操作，比如改文件、跑测试 | `log` 记下做了什么和结果 |
| 用户改了方向 | 改步骤，或者 `goal drop` 写明原因 |
| 全部做完 | 想一想学到了什么，有价值就写成经验节点，再 `goal done` 写结果 |

只需要一步的小事不用建目标。

## 例子

```memweave
goal create publish-mcp "发布 MCP 版本"
  desc """把 MCP 服务发布到 crates.io，做到别人能安装并连接。"""
  step "跑完回归"
  step "升级版本号"
  step "发布"
end
```

做完第一步，顺便记下结果。一段脚本可以写多条：

```memweave
goal create g "目标"
  desc """描述"""
  step "跑完回归"
  step "升级版本号"
end
step done g 1
log """回归全部通过：207 个测试。"""
```

卡住、插入、去掉步骤：

```memweave
goal create g "目标"
  desc """描述"""
  step "一"
  step "二"
end
step block g 1 """等用户确认版本号"""
step add g "补一份变更说明" after 1
step remove g 2
```

用户不要了：

```memweave
goal create g "目标"
  desc """描述"""
  step "一"
end
goal drop g """用户说这个版本先不发。"""
```

还有没完成的步骤时不能结束，报 `E221`：

```memweave-fails E221
goal create g "目标"
  desc """描述"""
  step "一"
end
goal done g """结果"""
```

## 工作板长什么样

```text
工作板
goal publish-mcp "发布 MCP 版本"
  desc: "把 MCP 服务发布到 crates.io，做到别人能安装并连接。"
  [x] 1 "跑完回归"
  [>] 2 "升级版本号"
  [ ] 3 "发布"
  [!] 4 "写公告" 卡住：等用户确认
最近行为
  2026-10-31T11:52Z step done publish-mcp 1
总表
section "MemWeave 的实现"
  desc: "..."
  no-memory-pool "节点用 new 分配，不做内存池"
```

`[x]` 已完成，`[>]` 进行中，`[ ]` 没开始，`[!]` 卡住。目标结束后离开工作板，留在时间线上。

## 时间线

记忆的每次修改都由系统记下时间，你改不了。时间是 UTC。`log` 记下的内容也在上面。

```memweave
timeline limit 20
timeline node drop
```

| 想知道什么 | 怎么做 |
| --- | --- |
| 这条记忆什么时候写的、最后改过没有 | `show` 里的 `created` 和 `updated`。`未知` 表示系统没有记录 |
| 这条记忆经历过什么 | `timeline node 节点` |
| 之前做过什么、这件事做过没有 | `timeline`，没看完就按提示用 `offset` |

## 经验

做事学到的东西是知识，写成普通节点，放在 `经验` 板块里，这样下次走图能找到。没有这个板块就 `catalog create` 新建。

```memweave
catalog create "经验" desc """做事时学到的教训：什么做法有效、什么做法踩过坑。"""
source mcp-trial-verifier
  title "试验校验太死时会误判"
  desc """DeepSeek 试验失败时先看模型实际做了什么，可能是校验写错了，不一定是模型错了。"""
  text """试验输出：失败 | 空记忆开始 | 模型记了两条，校验要求只能有一个节点。"""
end
catalog add mcp-trial-verifier to "经验"
```

原文要用真实发生的内容，比如用户的话、工具的输出。没有真实内容就不是经验，只是猜测。
