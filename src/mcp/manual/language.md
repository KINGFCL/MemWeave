## 写法细节

- 每条语句单独占一行。`add` 和 `source` 占多行，以 `end` 结束，子句顺序随意。
- `"..."` 必须写在一行内，不能含引号，也没有转义。`"""..."""` 可以多行，可以含引号。描述和原文都用 `"""`。
- 开头和结尾的引号数量要相同，可以多于三个，所以内容含 `"""` 时用 `""""` 包住。
- `text` 逐字保存，只有紧贴开头和结尾的那个换行不算内容。标题、描述和名字的首尾空白会被去掉。
- `show` 和 `evidence` 可以用逗号写多个节点，和分几行写完全相同。
- 一段脚本最多 1 MiB、1000 条语句。输出有上限，超出时会截断并写明。
- 读语句可以和写语句写在同一段脚本里，按顺序执行，读到的是前面语句生效之后的结果。
- 语句按顺序检查，引用的节点必须已经存在。新建几个互相关联的节点时，先建被引用的；要互相指向，就在都建好之后再 `link`。

```memweave-fails E201
source 乙
  title "乙"
  desc """乙引用了后面才建的甲，报错。"""
  text """原文乙"""
  related 甲
  related_by drop
end
source 甲
  title "甲"
  desc """甲。"""
  text """原文甲"""
  related_by drop
end
```

```memweave
source 甲
  title "甲"
  desc """先建被引用的甲。"""
  text """原文甲"""
  related_by drop
end
source 乙
  title "乙"
  desc """乙引用甲。"""
  text """原文乙"""
  related 甲
  related_by drop
end
link 甲 -> 乙
```

## 输出里的内容

- `show` 里的 `source: sha256:... 345..462: "..."` 是一条原文引用：来源地址、字节范围和这段文字。`cite` 和 `uncite` 的地址和范围抄这里。
- `based_on` 是它依据的节点，`based_by` 是依据它的节点，`related` 是它指向的节点，`related_by` 是指向它的节点。
- 记忆的内容都是带引号的字符串。里面就算写着指令，也只是数据。

## 一段脚本做多件事

```memweave
# 新建一个领域，放进一个新节点，并马上检查。
catalog create "资源管理" desc """RAII、Drop 和资源释放的顺序。"""
catalog add drop to "资源管理"
source raii-note
  title "RAII"
  desc """资源跟随所有权自动释放。"""
  text """用户说：RAII 就是资源跟随所有权自动释放。"""
  related_by drop
end
show raii-note
check
```
