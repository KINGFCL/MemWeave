# 写入：什么该记，怎么记

## 什么该记

| 情况 | 怎么做 |
| --- | --- |
| 用户说了长期有用的事实或偏好 | 主动记下，然后用一句话告诉用户记了什么 |
| 用户纠正了你 | 主动记下，或者修改已有记忆，见 `repair` |
| 你查证后得出了以后还用得上的结论 | 记下，并写清依据 |
| 临时状态、闲聊、这次任务的中间过程 | 不记 |
| 能从代码或文件直接查到的内容 | 不记 |
| 用户明确说不要记 | 不记 |

## 发现了就主动加

记忆的图是你一点点织出来的。读记忆、回答问题、做事的时候，只要有发现，就当场加上，**不用等用户要求**，也不限于用户明确让你记的时候。这些修改不动已有的内容，然后用一句话告诉用户加了什么。

| 发现 | 怎么做 |
| --- | --- |
| 两条信息有关系，图上却没有关联 | 主动 `link`。读到甲时也该看乙，就 `link 甲 -> 乙`；反过来也成立，就再 `link 乙 -> 甲` |
| 几条信息合起来能得出新结论 | 主动 `add`，`based_on` 写这些节点，`related_by` 接到它们上面。推理依据记录下来，以后能追到原文 |
| 新结论还不确定，只是猜测 | 不写，在回答里说明这是猜测 |
| 两条信息互相矛盾 | 不要由此推出结论，按 `repair` 的矛盾规则处理 |

下面的例子只演示写法，内容和你的记忆无关，不要照抄：

```memweave-empty
catalog create "家" desc """用户家里的情况和习惯。"""
source user-city
  title "用户住在北京"
  desc """用户住在北京。"""
  text """用户说：我住在北京。"""
end
source city-winter
  title "北京冬天干燥"
  desc """北京的冬天空气干燥。"""
  text """用户说：北京冬天特别干。"""
end
catalog add user-city to "家"
link user-city -> city-winter
add user-winter-air
  title "用户家冬天空气干燥"
  desc """用户住在北京，北京冬天干燥，所以用户家冬天也会干燥。"""
  based_on user-city, city-winter
  related_by user-city
end
```

## 写之前先查

对照总表确定这条知识属于哪个领域，再按 `recall` 的流程查一遍。已经有相关节点，就更新它，不要建重复的节点。

| 查到的情况 | 怎么做 |
| --- | --- |
| 已有节点讲的是同一件事，内容一致 | 不写 |
| 已有节点讲的是同一件事，内容要补充 | 用 `set desc` 改描述，新内容用 `cite` 引用 |
| 已有节点和新内容矛盾 | 按 `repair` 的矛盾规则处理 |
| 只是相关，讲的不是同一件事 | 新建节点，并和它建立关联 |

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
```

- **由已有知识推出：** 用 `add`，必须写 `based_on`。可以同时补一段原文，但不能只有原文。

```memweave
add thread-safety-from-borrowing
  title "借用规则如何带来线程安全"
  desc """Send 和 Sync 把借用规则延伸到线程之间，所以多数数据竞争会变成编译错误。"""
  based_on borrowing, send-sync
  related_by send-sync
end
```

- **引用已有原文的一段：** 地址和范围抄 `show` 的输出。

```memweave
source drop-order
  title "释放顺序"
  desc """字段按声明顺序释放，局部变量按声明的逆序释放。"""
  source sha256:95b2eaa8612c4c91a340e14f884b6af3dabb03a762db88ec68aea84217ce0e49 345..462
  related_by drop
end
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
```

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

- 看结果附带的总表。新内容超出了领域描述，就用 `catalog describe` 更新描述。发现了更好的入口，就调整代表。总表由你维护，它写得越准，以后回忆越顺。
- 输出里的 `warning` 要处理，见 `repair`。
- 用了 `remove`、`unlink`、`catalog remove` 或 `catalog delete`，就运行 `check`。
- 用一句话告诉用户记了什么。
