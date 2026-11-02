## 删除

只有用户要求删除，或者已经确认这条记忆错误且没有保留价值时才删。内容只是过时、但还有依据的，优先改而不是删。

1. `revision based_by` 查有没有结论依据它。
2. 有的话，先给它们换依据，或者一起删除。还有依据时直接删除会报 `E213`。
3. `remove`，然后 `revision check`。

这些都可以写在一段脚本里：

```memweave
revision based_by borrowing
base send-sync on ownership
unbase send-sync on borrowing
remove borrowing
revision check
save
```

删除只会清掉指向它的关联，不会改写别的节点。枢纽节点删除后，可能有节点走不到，要接回去：

```memweave
link borrowing -> drop
remove ownership
revision check
save
```

## 处理 `revision check` 的结果

| 输出 | 怎么做 |
| --- | --- |
| `从总表到达不了 x` | 有用：`link 某节点 -> x`，或者 `catalog add x to "板块"`。确认没用：`remove x` |
| `板块 "x" 没有代表节点` | 还需要这个领域：`catalog add`。不需要了：`catalog delete` |
| `原文 x 丢失或不完整` | 告诉用户可以运行 `memweave doctor`。有别的真实内容，就用 `cite` 换掉，并 `uncite` 旧的 |
| `check: 没有问题` | 结束 |

`revision check` 只管找不找得到，不判断内容对不对。

## 改名

`id` 不能改。要换地址，就新建节点，把关联和依据换过去，再删除旧节点。`title` 可以直接用 `set` 改，总表里的标题会同步。
