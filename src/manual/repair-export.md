## 导出

导出会把当前记忆写成一个新的知识库目录，交给别的 agent 或用来备份。

```memweave
revision check
revision export "备份/记忆"
```

- 目标必须不存在或是空目录，不会覆盖，否则报 `E216`。
- 有节点走不到时报 `E217` 并全部列出，整段脚本不执行。接回去或删除后再导出，可以写在同一段脚本里，写在 `revision export` 前面。
- 没有节点引用的原文不会导出。
- 只读工具不能导出。

```memweave-fails E217
unlink ownership -> drop
revision export "备份/记忆"
```

```memweave
unlink ownership -> drop
catalog add drop to "Rust 内存管理"
revision export "备份/记忆"
save
```
