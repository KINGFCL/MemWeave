## 新节点放在哪

新节点必须能被找到。在同一段脚本结束前，它必须接入图，否则报 `E208`，整段不执行。新节点只是互相关联不算接入。

| 情况 | 怎么做 |
| --- | --- |
| 领域里有相关的节点 | 用 `related_by` 接到最相关的节点上。这个节点是从总表走到新节点的路 |
| 新节点和别的节点也相关 | 用 `related` 指向它们。读到新节点时，它们也该看 |
| 领域里没有相关的节点，代表不到 5 个 | `catalog add` 让它当代表 |
| 领域里没有相关的节点，代表已经满了 | 建一个概括性的节点接替代表的位置，把这些知识都接到它下面，见下面的例子 |
| 它开了一个新领域 | `catalog create` 建板块并写清描述，再 `catalog add` |

**同一段脚本新建几个节点时，先建被引用的。** 语句按顺序检查，引用后面才建的节点会报 `E201`。要互相指向，就都建好之后再 `link`。

**不要为了能被找到，就把它挂到不相关的节点下面。** 假关联会带偏以后的回忆。

**总表不是索引，不要把每个新节点都放进总表。** 板块的描述要写清这个领域包含什么，以后选领域全靠它。

**代表要选最能概括这个领域的节点，不是最先写的节点。** 从代表出发，要能看出这个领域还有什么。第一个节点不能概括领域时，等领域大一些，就补一个概括性的节点，把它换成代表：

```memweave
add rust-memory-overview
  title "Rust 内存管理总览"
  desc """所有权决定谁负责释放，借用决定谁能访问，Drop 决定释放时做什么。"""
  based_on ownership, borrowing, drop
  related ownership, borrowing, drop
end
catalog add rust-memory-overview to "Rust 内存管理"
catalog remove borrowing from "Rust 内存管理"
check
```

## 从空记忆开始

总表显示 `catalog: 空` 时，从第一个领域开始建：

```memweave-empty
catalog create "咖啡" desc """咖啡的产地、烘焙、冲煮和风味。"""
source hand-brew-temperature
  title "手冲水温"
  desc """用户手冲浅烘豆时习惯用 92 度的水。"""
  text """用户说：我手冲浅烘豆一般用 92 度的水。"""
end
catalog add hand-brew-temperature to "咖啡"
```

## 写完之后

- 对照总表。新内容超出了领域描述，就用 `catalog describe` 更新描述。发现了更好的入口，就调整代表。总表由你维护，它写得越准，以后回忆越顺。
- 输出里的 `warning` 要处理，见 `manual-repair`。
- 用了 `remove`、`unlink`、`catalog remove` 或 `catalog delete`，就运行 `check`。
