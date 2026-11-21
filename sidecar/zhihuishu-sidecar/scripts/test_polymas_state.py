"""回归测试: polymas 课程状态解析(挂机脚本的纯逻辑部分).

覆盖(2026-11-21 生产机实测数据):
1. 「总学习进度 X%共 N 个学习资源」→ (百分比, 资源数); 读不到就是 None。
2. 资源行的**完成标记**: 已看完 = `.agent-course-form-item-checkbox success`,
   没看过 = `... info`。
   ⚠️ 注意别被勾选框骗了 —— 勾选框(el-checkbox is-checked)**全都**是勾上的,
   它不是完成标记, 完成标记是 checkbox 自身的 class(success / info)。
3. 只有视频(mp4 等)才算「能刷」, doc/pdf 跳过。

跑法(纯 stdlib, 宿主/CI 都能跑):
    python3 -u scripts/test_polymas_state.py
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from polymas_state import (  # noqa: E402
    parse_progress,
    parse_resource_rows,
    pick_pending_videos,
)

FAILED: list[str] = []


def check(cond: bool, label: str) -> None:
    if cond:
        print(f"  ok   {label}")
    else:
        print(f"  FAIL {label}")
        FAILED.append(label)


# ---- 进度解析 ---------------------------------------------------------------

check(parse_progress("总学习进度 0%共 5 个学习资源") == (0, 5),
      "「总学习进度 0%共 5 个学习资源」→ (0, 5)")
check(parse_progress("总学习进度 20%共 5 个学习资源") == (20, 5),
      "「总学习进度 20%共 5 个学习资源」→ (20, 5)")
check(parse_progress("总学习进度 100%共 1 个学习资源") == (100, 1),
      "进度 100% 也能读")
check(parse_progress("加载中") == (None, None), "读不到 → (None, None)")
check(parse_progress("") == (None, None), "空文本 → (None, None)")
check(parse_progress("总学习进度 0%") == (0, None), "只有进度没有资源数 → (0, None)")


# ---- 资源行解析 -------------------------------------------------------------

# 真实抓到的两个原始片段(生产机 2026-11-21):
ACTIONS_DONE = (
    '<!----><!----><div class="check"><!----><div class="el-checkbox-group" '
    'role="group" aria-label="checkbox-group" id="el-id-1490-43">'
    '<label class="el-checkbox is-checked agent-course-form-item-checkbox success w-full">'
    '<span class="el-checkbox__input is-checked">'
    '<input class="el-checkbox__original" type="checkbox" value="checked" tabindex="-1">'
    '<span class="el-checkbox__inner"></span></span>'
    '<span class="el-checkbox__label"></span></label></div></div>'
)
ACTIONS_TODO = ACTIONS_DONE.replace("success w-full", "info w-full")

ICON_VIDEO = ("https://prod-polymas.oss-cn-hangzhou.aliyuncs.com/polymas-basic-resource/"
              "YkX3ld/UugXaA_RoEV0G/686e3053e4b0a2a97a9179f9.png")
ICON_DOC = ("https://prod-polymas.oss-cn-hangzhou.aliyuncs.com/polymas-basic-resource/"
            "YkX3ld/UugXaA_RoEV0G/686e307ae4b0a2a97a9179fb.png")
ICON_PDF = ("https://prod-polymas.oss-cn-hangzhou.aliyuncs.com/polymas-basic-resource/"
            "YkX3ld/UugXaA_RoEV0G/686e30a0e4b0a2a97a9179fe.png")

ROWS = [
    # 看过的一个 mp4
    {"name": "《大学生国家安全教育》数字教材阅读.mp4", "icon": ICON_VIDEO,
     "actions": ACTIONS_DONE},
    # 没看过的 mp4
    {"name": "统筹发展和安全的科学内涵.mp4", "icon": ICON_VIDEO,
     "actions": ACTIONS_TODO},
    # 文档(不是视频)
    {"name": "附件二（视频报告专用）：思想政治理论课实践教学学生视频报告模板(2)(1).doc",
     "icon": ICON_DOC, "actions": ACTIONS_TODO},
    # 另一个没看过的 mp4
    {"name": "资源安全的重要性.mp4", "icon": ICON_VIDEO, "actions": ACTIONS_TODO},
    # pdf
    {"name": "26-27GE1059《国家安全教育》考核作业纸.pdf", "icon": ICON_PDF,
     "actions": ACTIONS_TODO},
]

resources = parse_resource_rows(ROWS)
check(len(resources) == 5, f"解析出 5 条资源, 实得 {len(resources)}")
check(resources[0]["done"] is True, "success 标记 = 已完成")
check(resources[1]["done"] is False, "info 标记 = 未完成")
check(resources[0]["is_video"] is True, "mp4 是视频")
check(resources[2]["is_video"] is False, "doc 不是视频")
check(resources[4]["is_video"] is False, "pdf 不是视频")
check(resources[1]["name"] == "统筹发展和安全的科学内涵.mp4", "名字原样保留")

pending = pick_pending_videos(resources)
check([r["name"] for r in pending] == [
    "统筹发展和安全的科学内涵.mp4", "资源安全的重要性.mp4"],
    f"待刷清单只含未看过的视频, 实得 {[r['name'] for r in pending]}")

# 名字里没有扩展名但图标是视频的, 也算视频(平台改过文件名的情况)
icon_only = parse_resource_rows([{"name": "某视频资源", "icon": ICON_VIDEO,
                                   "actions": ACTIONS_TODO}])
check(icon_only[0]["is_video"] is True, "图标是视频图标 → 算视频")

# 空名字的行要丢掉(文件夹行没有 .file-name, 但防御一下)
check(len(parse_resource_rows([{"name": "  ", "icon": None, "actions": ""}])) == 0,
      "空名字的行被丢掉")


if FAILED:
    print(f"\n✗ {len(FAILED)} 项断言失败:")
    for item in FAILED:
        print(f"  - {item}")
    sys.exit(1)
print("\n✓ 全部通过")
