## 新节点怎么写

- **一个节点只讲一件事。** 想用“以及”连接两件事时，就拆成两个节点。
- **`id` 要稳定，并且能看出内容**，比如 `hand-brew-temperature`。创建后不能改。只能用字母、数字、汉字、`-` 和 `_`。
- **`desc` 写你的理解，要能独立读懂。** 不要写“如上所述”，以后读到它时看不到现在的上下文。
- **原话放进 `text`，理解放进 `desc`。** `desc` 的首尾空白会去掉，`text` 逐字保存。
- **`text` 只放真实的原文，并且逐字照抄：** 用户的原话、文件或工具返回的内容，可以在前面加“用户说：”。不要转述，也不要补充对方没说的话。你自己的推断、总结和修正都不是原文，写进 `text` 就是伪造证据。你自己推出的结论用 `add`，依据写 `based_on`。

## 选 `source` 还是 `add`

- **直接来自原文或用户原话：** 用 `source`，必须有原文，不能写 `based_on`。

```memweave
source arc-atomic
  title "Arc 的引用计数是原子的"
  desc """Arc 用原子操作维护引用计数，所以可以跨线程共享。"""
  text """用户说：Rust 的 Arc 用原子操作维护引用计数，所以可以跨线程共享。"""
  related send-sync
  related_by ownership
end
save
```

- **由已有知识推出：** 用 `add`，必须写 `based_on`。可以同时补一段原文，但不能只有原文。

```memweave
add thread-safety-from-borrowing
  title "借用规则如何带来线程安全"
  desc """Send 和 Sync 把借用规则延伸到线程之间，所以多数数据竞争会变成编译错误。"""
  based_on borrowing, send-sync
  related_by send-sync
end
save
```

- **引用已有原文的一段：** 地址和范围抄 `revision show` 的输出。

```memweave
source drop-order
  title "释放顺序"
  desc """字段按声明顺序释放，局部变量按声明的逆序释放。"""
  source sha256:95b2eaa8612c4c91a340e14f884b6af3dabb03a762db88ec68aea84217ce0e49 345..462
  related_by drop
end
save
```

## 怎么写 `text`

`text` 逐字保存。内容可以从开头引号的下一行写起，紧贴开头和结尾的那个换行不算内容，其他空白和缩进都算。内容含三个连续的引号时，用四个引号包住，开头和结尾的引号数量要相同。

```memweave
source python-doc-example
  title "用户贴的 Python 函数"
  desc """用户给的例子：带文档字符串的函数，返回 1。"""
  text """"
def f():
    """文档"""
    return 1
""""
  related_by ownership
end
save
```
