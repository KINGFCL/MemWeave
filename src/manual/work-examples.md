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
