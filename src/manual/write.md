## 什么该记

| 情况 | 怎么做 |
| --- | --- |
| 用户说了长期有用的事实或偏好 | 适合记 |
| 用户纠正了你 | 适合记，或者修改已有记忆，见 `manual-repair` |
| 你查证后得出了以后还用得上的结论 | 适合记，并写清依据 |
| 临时状态、闲聊、这次任务的中间过程 | 不记 |
| 能从代码或文件直接查到的内容 | 不记 |
| 用户明确说不要记 | 不记 |

## 发现关系和结论时怎么加

记忆的图是一点点织出来的。读记忆、回答问题、做事的时候可能发现新的关系或结论。要不要主动加，由接入你的 agent 决定，按系统提示和用户的要求做。要加的时候这样做，这些修改不动已有的内容：

| 发现 | 怎么做 |
| --- | --- |
| 两条信息有关系，图上却没有关联 | 用 `link`。读到甲时也该看乙，就 `link 甲 -> 乙`；反过来也成立，就再 `link 乙 -> 甲` |
| 几条信息合起来能得出新结论 | 用 `add`，`based_on` 写这些节点，`related_by` 接到它们上面。推理依据记录下来，以后能追到原文 |
| 新结论还不确定，只是猜测 | 不写，在回答里说明这是猜测 |
| 两条信息互相矛盾 | 不要由此推出结论，按 `manual-repair-conflict` 的矛盾规则处理 |

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
