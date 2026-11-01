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
