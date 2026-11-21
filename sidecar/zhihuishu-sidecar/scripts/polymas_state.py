"""polymas(南京大学 AI教学中心)课程状态解析 —— 纯函数, 不依赖浏览器.

为什么单独成文件(2026-11-21):
挂机脚本要在**没有浏览器**的情况下也能被测(单测跑在宿主/CI 上, 容器里才有
playwright)。所以页面取回来的原始数据在这里解析, 浏览器那边只负责取。

实测到的页面事实(生产机, 2026-11-21):
- 课程页「我的学习 → 学习资源」里每条资源一个 ``.el-tree-node__content``:
    .node-left   → 类型标签(必学/选学) + 文件类型图标(img.file-icon) + .file-name(文件名)
    .node-actions → 完成标记: ``.agent-course-form-item-checkbox success`` = 已完成,
                    ``... info`` = 未完成(不是勾选框! 勾选框全是 is-checked)
- 页面顶部「总学习进度 X%共 N 个学习资源」→ 课程整体进度与资源总数。
- 看完一个 4 分 07 秒的 mp4 后, 进度从 0% 变 20%(5 个资源完成 1 个) → 平台认「看过了」。
"""
from __future__ import annotations

import re

# 视频扩展名(资源名以此结尾才算「能刷」的视频)
VIDEO_EXTENSIONS = (
    ".mp4", ".avi", ".mov", ".flv", ".mkv", ".wmv", ".webm", ".m4v", ".mpeg", ".mpg",
)

# 完成标记的 class: 完成 = success, 未完成 = info
DONE_CLASS_MARK = "agent-course-form-item-checkbox success"

# 文件类型图标: 视频图标的文件名(obs 阿里云上的固定串, 变了也只是少认一类,
# 不影响 —— 扩展名是第一判据)
VIDEO_ICON_MARK = "686e3053e4b0a2a97a9179f9.png"

_PROGRESS_RE = re.compile(r"总学习进度\s*(\d{1,3})\s*%")
_COUNT_RE = re.compile(r"共\s*(\d+)\s*个学习资源")


def parse_progress(text: str) -> tuple[int | None, int | None]:
    """页面文本 → (总学习进度百分比, 资源总数); 读不到就是 None."""
    pm = _PROGRESS_RE.search(text)
    cm = _COUNT_RE.search(text)
    return (
        int(pm.group(1)) if pm else None,
        int(cm.group(1)) if cm else None,
    )


def parse_resource_rows(rows: list[dict]) -> list[dict]:
    """原始资源行 → 结构化资源列表.

    rows 每项形如 {"name": str, "icon": str|None, "actions": str|None}
    (由页面里 .el-tree-node__content 逐个抓出来)。

    返回每项: {name, done, is_video, icon}
    - done: True = 平台已记为完成(挂机要跳过, 否则每天重复刷同样的视频)
    - is_video: 能不能当视频播(扩展名是视频后缀, 或图标是视频图标)
    """
    out: list[dict] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        name = (row.get("name") or "").strip()
        if not name:
            continue
        actions = row.get("actions") or ""
        icon = row.get("icon") or ""
        lower = name.lower()
        out.append(
            {
                "name": name,
                "done": DONE_CLASS_MARK in actions,
                "is_video": lower.endswith(VIDEO_EXTENSIONS)
                or VIDEO_ICON_MARK in icon,
                "icon": icon or None,
            }
        )
    return out


def pick_pending_videos(resources: list[dict]) -> list[dict]:
    """挑出「还没看过」的视频资源(挂机的实际目标)."""
    return [r for r in resources if r.get("is_video") and not r.get("done")]
