# 草稿：改完检查，再保存

所有修改指令都只改草稿，包括节点、总表、工作板和 `log`。修改马上能读到，但只有 `save` 之后才算保存。

| 指令 | 作用 |
| --- | --- |
| `revision 读语句` | 读当前草稿，平时都用它。没有草稿时就是已保存的版本 |
| `origin 读语句` | 读已保存的版本，看不到未保存的修改 |
| `revision display` | 有没有未保存的修改、改了哪些，不带内容 |
| `revision display-detail` | 具体改了什么，新节点显示全部内容 |
| `comparison display` | 已保存的版本和草稿逐项对照，只列出变了的字段 |
| `comparison display-diff` | 和上面一样，但标题和描述逐字比较，只显示改了的字词，列表只列增减 |
| `save` | 保存全部修改。可以单独执行，也可以写在脚本最后一行 |
| `discard` | 放弃全部未保存的修改，要单独执行 |

这四个看修改的指令后面都可以写节点，多个用逗号分开，比如 `comparison display-diff drop, ownership`，只看这些节点。第一行都是 `has_revision`。

写的节点没有改动时显示 `节点 没有改动`，地址写错时显示 `节点 不存在`，都不算报错，别的节点照常显示。没有未保存的修改时，只有一行 `has_revision: false`。

## 什么时候用

| 情况 | 怎么做 |
| --- | --- |
| 新节点还没接到图上 | 可以先放着，分几次调用写。`save` 时必须走得到，否则报 `E208` |
| 改动很小，有把握 | 脚本最后一行写 `save`，一次完成 |
| 改动大，或者要分几次调用才能改完 | 先改，用 `revision check` 和 `comparison display` 检查，没问题再 `save` |
| 想知道改了哪些 | `revision display` |
| 只是在描述末尾补一句 | 用 `append 节点 desc`，不要用 `set` 重写全文 |
| 只改了几个字，想看改在哪 | `comparison display-diff 节点` |
| 想对比改之前和改之后的完整文字 | `comparison display`。要看某个节点没变的部分，再用 `origin show 节点` 或 `revision show 节点` |
| 改错了，想全部重来 | `discard` |
| 结果里出现 `revision: N 段脚本未保存` | 还有修改没保存。做完了就 `save`，不要了就 `discard`，不要放着不管 |
| 刚开始工作就看到这行提示 | 这是上次留下的。先 `comparison display` 看内容，能确认就 `save`，不能确认就问用户 |

要结束工作时，确认 `revision display` 的第一行是 `has_revision: false`。忘记保存的修改不算数。

## 例子

一次改完并保存：

```memweave
set drop title "Drop：值离开作用域时运行"
save
```

先改、检查、对比，确认后再保存：

```memweave
set drop desc """值离开作用域时运行 Drop。"""
revision check
revision display
comparison display
save
```

`revision display` 的输出。`+` 是新增，`-` 是删除，`~` 是改动，后面写着改了哪几项：

```text
has_revision: true
scripts: 3
+ raii-note "RAII"
~ drop: desc, related
- old-note "旧笔记"
~ section "资源管理": 代表
修改记录
  2026-11-02T08:00Z source raii-note "RAII"
```

`comparison display` 的输出：

```text
has_revision: true
scripts: 3
~ drop
  desc
    origin:   "旧描述"
    revision: "新描述"
  related
    origin:   无
    revision: raii-note
+ raii-note
  origin:   没有这个节点
  revision:
    title: "RAII"
    desc: "资源跟随所有权自动释放。"
    related: 无
    source: sha256:... 0..59: "用户说：RAII 就是资源跟随所有权自动释放。"
- old-note
  origin:
    title: "旧笔记"
    desc: "……"
    related: 无
    source: sha256:... 0..30: "……"
  revision: 已删除
```

`comparison display-diff` 的输出。`=` 是没变的文字，只留改动前后几个字，`-` 是删掉的，`+` 是加上的，所有内容都是字符串：

```text
has_revision: true
scripts: 3
~ drop
  desc
    （省略 42 字）
    = "局部变量按声明的逆序释放"
    - "。"
    + "，顺序不能改。"
  related
    + raii-note
```

把 `=` 和 `-` 的内容连起来就是原来的文字，把 `=` 和 `+` 连起来就是现在的文字，省略的部分两边相同。改动特别大时不会逐字比较，而是写明“内容太长，没有逐字比较”，显示两边全文。

只列出你能写的内容，反向列表不算。输出太长被截断时，第一行还在，按提示分开看。

## 要注意

- 没保存的修改记在草稿文件里，进程重启后还在，但断电可能丢失。
- `revision export` 导出当前草稿，`origin export` 导出已保存的版本。`discard` 不会删除已经导出的目录。
- 放弃之后，草稿里新存的原文文件还在磁盘上，要到下次 `memweave compact` 才删除。
- 同一时间只有一份草稿，连接这份记忆的调用都在改它。
