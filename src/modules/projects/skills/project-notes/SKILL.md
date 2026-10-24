---
name: project-notes
description: 管理用户的项目库（02-Projects-项目）。当用户问「我的项目」「XX 项目进展」「最近项目动态」、要求新建/归档/更新项目笔记时使用。GitHub 项目的 README 与仓库元数据由 ru41 projects 周期同步到各项目目录的 Repo-GitHub.md（只读），项目立场与进度以人写的笔记为准。
---

# 项目库管理

## 结构

`02-Projects-项目/<项目名>/`（平铺，无分组目录）：

- 项目目录里**至少一个笔记**带 frontmatter 的 `repo:` 行（GitHub 链接）即成为 GitHub 项目，每日自动同步 `Repo-GitHub.md`（README + stars/pushed_at 等元数据，机器所有勿手改）
- 无 `repo:` 行的项目纯笔记管理，同样有效
- 项目停了/黄了：笔记 frontmatter 加 `status: archived`，不用挪目录

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
- 项目黄了/停了：frontmatter 标 `status: archived`，GitHub 项目依旧会同步元数据
- 回答项目问题时区分「仓库事实」（Repo-GitHub.md）与「用户立场」（项目笔记），注明来源
