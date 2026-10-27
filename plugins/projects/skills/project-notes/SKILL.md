---
name: project-notes
description: 管理用户的项目库（02-Projects-项目）。当用户问「我的项目」「XX 项目进展」「最近项目动态」、要求新建/归档/更新项目笔记时使用。GitHub 项目的 README 与仓库元数据由 ru41 projects 周期同步到各项目目录的 Repo-GitHub.md（只读），项目立场与进度以人写的笔记为准。
---

# 项目库管理

## 结构

`02-Projects-项目/<分组>/<项目名>/`，两个分组：

- `Active-进行中/`：活跃项目 · `Archived-已归档/`：GitHub 最后更新超过 180 天（或仓库已 archived）自动归入，恢复活跃自动移回
- 账号同步：配置了 github_user 后，账号全部公开仓库每日自动建目录，`README.md`（元数据 frontmatter + README 正文，机器所有勿手改）
- 手工项目：目录里放一个带 frontmatter `repo:` 行的笔记即成为锚点项目（适合非本账号仓库）；无锚点的纯笔记项目（如挖洞记录）手工维护，同步器不碰

## 新建项目笔记模板

```markdown
---
repo: https://github.com/<owner>/<repo>   # GitHub 项目才需要这行
---

# <项目名>

## 目标

## 当前进度

## 下一步
```

写入 vault（只读挂载下让用户确认，或经 `ru41 obsidian` 写入通道）；直接在 02-Projects-项目 下建项目目录。

## 查询

- 项目现状：`rg --files "/vault/02-Projects-项目"` 列目录；读对应 `Repo-GitHub.md` 的 frontmatter 看 pushed_at/stars/archived 判断仓库活没活着
- 技术细节问 README 正文（同文件）；你的立场与计划读人写的项目笔记
- 手动刷新同步：`ru41 --config /etc/ru41/config.toml --db /data/state.db projects poll`

## 纪律

- 进度、结论、复盘写进**项目笔记**（人写文件），不写 Repo-GitHub.md（机器覆写会清掉）
- 分组由同步器按 pushed_at 自动维护，不要手工把账号项目挪组；手工项目想归档自己挪到 Archived-已归档/
- 回答项目问题时区分「仓库事实」（Repo-GitHub.md）与「用户立场」（项目笔记），注明来源
