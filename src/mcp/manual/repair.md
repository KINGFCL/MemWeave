# 修复：纠正、矛盾、删除、导出

## 纠正

| 情况 | 怎么做 |
| --- | --- |
| 描述写错了，原文没错 | `set 节点 desc`，原文不动 |
| 原文说错了或过时了 | 存新原文，换掉引用，需要时改描述 |
| 用户纠正了你，但记忆里没有这件事 | 按 `write` 新建 |

换原文写在一段脚本里，**`cite` 要写在 `uncite` 前面**，否则节点中途没有证据，报 `E207`。旧原文文件不会删除，只是不再被引用。新原文必须是用户的原话或别的真实内容，不能是你自己写的结论。

```memweave
cite borrowing text """用户纠正：同一时刻可以有多个共享引用，或者一个可变引用，两者不能并存。"""
uncite borrowing source sha256:95b2eaa8612c4c91a340e14f884b6af3dabb03a762db88ec68aea84217ce0e49 161..344
set borrowing desc """同一时刻可以有多个共享引用，或者一个可变引用，两者不能并存。"""
```

写反了会被拒绝：

```memweave-fails E207
uncite borrowing source sha256:95b2eaa8612c4c91a340e14f884b6af3dabb03a762db88ec68aea84217ce0e49 161..344
cite borrowing text """新原文"""
```

## 处理“可能需要复查”

`set desc`、`cite`、`uncite` 之后，输出可能出现这样的 warning：

```text
warning: send-sync 依据 borrowing，证据变了，它可能需要复查
```

这个提醒只出现这一次，不会保存，所以**必须当场处理**：

1. `show` 那个结论，看它的描述和依据。
2. 还成立：不用改，告诉用户查过了。
3. 描述要调整：`set desc`。
4. 不再成立：换依据（`base`、`unbase`），或者按下面的规则删除。
5. 改了它之后如果又出现 warning，继续往下处理，直到没有新的 warning。

## 两条记忆互相矛盾

不要悄悄选一个，也不要顺手改掉。

1. 用 `evidence` 查双方的依据，需要时用 `full_source` 看上下文。看 `show` 里的 `created` 和 `updated` 判断新旧，并告诉用户。新的不一定对，但这是重要线索。
2. **用户只是提问，或者没有告诉你哪个对：不改记忆。** 在回答里说明存在矛盾，列出双方的说法和证据，问用户哪个对。你自己知道答案也一样：可以写在回答里，并说明这是你自己的判断，不是记忆里的内容。
3. **用户告诉了你哪个对：** 按上面的纠正流程改掉错的一方，把用户的原话用 `cite` 存成新原文。
4. **用户要你修复，而且记忆里的原文能判断对错：** 比如一方只是读错了原文。改掉错的描述，用 `cite 节点 source` 引用已有的正确原文，不要新写原文。
5. 记忆里的证据判断不了：问用户。**不能为了修复而编造原文。**

## 删除

只有用户要求删除，或者已经确认这条记忆错误且没有保留价值时才删。内容只是过时、但还有依据的，优先改而不是删。

1. `based_by` 查有没有结论依据它。
2. 有的话，先给它们换依据，或者一起删除。还有依据时直接删除会报 `E213`。
3. `remove`，然后 `check`。

这些都可以写在一段脚本里：

```memweave
based_by borrowing
base send-sync on ownership
unbase send-sync on borrowing
remove borrowing
check
```

删除只会清掉指向它的关联，不会改写别的节点。枢纽节点删除后，可能有节点走不到，要接回去：

```memweave
link borrowing -> drop
remove ownership
check
```

## 改错了

没有撤销指令。用 `timeline node 节点` 查它经历过什么，再写新的修改改回来。

## 处理 `check` 的结果

| 输出 | 怎么做 |
| --- | --- |
| `从总表到达不了 x` | 有用：`link 某节点 -> x`，或者 `catalog add x to "板块"`。确认没用：`remove x` |
| `板块 "x" 没有代表节点` | 还需要这个领域：`catalog add`。不需要了：`catalog delete` |
| `check: 没有问题` | 结束 |

`check` 只管找不找得到，不判断内容对不对。

## 改名

`id` 不能改。要换地址，就新建节点，把关联和依据换过去，再删除旧节点。`title` 可以直接用 `set` 改，总表里的标题会同步。

## 导出

导出会把当前记忆写成一个新的知识库目录，交给别的 agent 或用来备份。

```memweave
check
export "备份/记忆"
```

- 目标必须不存在或是空目录，不会覆盖，否则报 `E216`。
- 有节点走不到时报 `E217` 并全部列出，整段脚本不执行。接回去或删除后再导出，可以写在同一段脚本里，写在 `export` 前面。
- 没有节点引用的原文不会导出。
- 只读工具不能导出。

```memweave-fails E217
unlink ownership -> drop
export "备份/记忆"
```

```memweave
unlink ownership -> drop
catalog add drop to "Rust 内存管理"
export "备份/记忆"
```
