"""刷课目标生成器: 从采集报告推出「今晚刷哪些课」, 写进 Autovisor 的 config.ini.

背景(2026-11-21):
- Autovisor **不会自己判断刷哪些课** —— 它只按 config.ini 的 `[course-url]` 段
  逐个跑(modules/configs.py: get_course_urls → Autovisor.py: for ... config.course_urls)。
  也就是说, 刷课目标一直是**人工维护的静态清单**。
- 采集器(collector.py)已经能枚举门户上的全部课程, 并且读得到每门课的
  「总学习进度」百分比(见 detect_course_state) → **「哪些课还没刷完」这个判据
  本来就有数据源, 只是没人用它**。

本脚本做三件事:
1. 读 data/report.json → 选出「有内容可刷(content_published) 且 没刷完(progress < 阈值)」的课;
2. 把选中的课程 URL 写进 config.ini 的 `[course-url]` 段(URL1=… URL2=…) ——
   **只改这一段的选项行, 注释与其余段落原样保留**(config.ini 里有防风控的关键注释,
   用 configparser 重写会把注释全丢了);
3. 顺带写 data/targets.json: 今晚刷哪些、各自还差多少 —— 给 Ru41 侧播报用。

跑法(宿主上即可, 纯 stdlib, 不需要浏览器):
    python3 scripts/plan_targets.py --dry-run     # 只报告不改文件
    python3 scripts/plan_targets.py               # 真写 config.ini + targets.json

参数:
    --report PATH      报告路径(默认 data/report.json)
    --config PATH      Autovisor 配置路径(默认 config.ini)
    --out PATH         目标清单输出(默认 data/targets.json)
    --threshold PCT    进度低于多少算「还没刷完」(默认 100)
    --max N            每晚最多刷几门(默认 3; 每门 limitMaxTime=30min, 3 门≈90 分钟)
    --replace          清掉 config.ini 里人工维护的 URL(默认保留并排在自动选的前面)
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from typing import Any
from urllib.parse import urlparse

# [course-url] 段里表示一条课程 URL 的选项行(URL1=… URL2=…)
URL_OPTION_RE = re.compile(r"^\s*URL\d+\s*=")
SECTION_RE = re.compile(r"^\s*\[(?P<name>[^\]]+)\]\s*$")


# ---- 刷课引擎 ---------------------------------------------------------------
#
# 一门课能不能刷, 由「刷课器认不认得这门课的页面」决定, 不是由「有没有内容」决定。
#
# 2026-11-21 生产机实测(首跑 + DOM 探测):
#   上游 Autovisor 认课**不靠域名, 靠 DOM 选择器**
#   (modules/lesson_navigation.py: wisdom/fusion/hike/legacy 四类),
#   四类都不命中 → RuntimeError「课程目录加载超时」→ 该门课失败 → **整轮 break**
#   (Autovisor.py: if outcome is CourseOutcome.FAILED: break) → 后面全刷不到。
#   → 「Autovisor 能不能刷这门课」= 采集器在课程页上有没有命中目录选择器,
#     这个结论采集器已经量出来了, 记在报告的 catalog 字段里。
#
#   polymas(南京大学 AI教学中心, 5 门课在那边): 点资源会**跳页**、播放器在
#   **iframe** 里, 而上游要求「视频与目录同页」(page.wait_for_selector("video")),
#   结构上刷不了 → 走我们自研的挂机脚本(sidecar/scripts/brush_polymas.py)。
#
ENGINE_AUTOVISOR = "autovisor"
ENGINE_POLYMAS = "polymas"
ENGINE_AISMART = "aismart"

# polymas 平台的课程域名(自研引擎认这个)
POLYMAS_HOSTS = ("hike-teaching-center.polymas.com",)
# 智慧树新版 AI 课(另一套结构, 自研引擎里另一份实现)
AISMART_HOSTS = ("ai-smart-course",)


def classify_engine(course: dict[str, Any]) -> str | None:
    """这门课该由哪个刷课引擎刷; None = 现在谁都刷不了(别放进清单).

    - catalog 有值 → 上游 Autovisor 认得这门课(采集器实测命中了目录选择器)
    - polymas 域名 → 自研挂机脚本(brush_polymas.py)
    - ai-smart 域名 → 自研挂机脚本(brush_aismart.py)
    """
    url = (course.get("course_home_url") or "").strip()
    host = ""
    if url:
        try:
            host = urlparse(url).netloc.lower()
        except ValueError:
            host = ""
    if course.get("catalog"):
        return ENGINE_AUTOVISOR
    if any(h in host for h in AISMART_HOSTS):
        return ENGINE_AISMART
    if any(h in host for h in POLYMAS_HOSTS):
        return ENGINE_POLYMAS
    return None


def read_manual_urls(text: str) -> list[str]:
    """从 config.ini 文本里取出 [course-url] 段已有的 URL(人工维护的那份).

    裸首页(路径只有 "/")不算课程 —— 那是当初登录测试留下的占位
    (「https://www.zhihuishu.com」)。Autovisor 拿到它会判定「课程未确认完成」
    并 **break 掉整轮**, 后面的课一节都刷不到, 所以必须剔掉。
    """
    urls: list[str] = []
    in_section = False
    for line in text.splitlines():
        if SECTION_RE.match(line):
            in_section = SECTION_RE.match(line).group("name").strip() == "course-url"
            continue
        if in_section and URL_OPTION_RE.match(line):
            value = line.split("=", 1)[1].strip()
            if value and not is_bare_homepage(value):
                urls.append(value)
    return urls


def is_bare_homepage(url: str) -> bool:
    """是不是只有站点根路径的网址(占位符, 不是课程页)."""
    try:
        path = urlparse(url).path
    except ValueError:
        return True
    return path in ("", "/")


def write_course_urls(text: str, urls: list[str]) -> str:
    """只替换 [course-url] 段的选项行, 其余内容(注释、其它段)原样保留."""
    lines = text.splitlines()
    out: list[str] = []
    in_section = False
    inserted = False
    for line in lines:
        section = SECTION_RE.match(line)
        if section:
            in_section = section.group("name").strip() == "course-url"
            out.append(line)
            if in_section and not inserted:
                out.extend(f"URL{i}={u}" for i, u in enumerate(urls, 1))
                inserted = True
            continue
        if in_section and URL_OPTION_RE.match(line):
            continue  # 旧的选项行: 丢掉, 由上面统一插入
        out.append(line)
    if not inserted:
        # 原来就没有 [course-url] 段 → 追加一段
        if out and out[-1].strip():
            out.append("")
        out.append("[course-url]")
        out.extend(f"URL{i}={u}" for i, u in enumerate(urls, 1))
    return "\n".join(out) + "\n"


def pick_courses(report: dict[str, Any], threshold: int, limit: int) -> list[dict[str, Any]]:
    """挑出今晚要刷的课: 有内容可刷 + 没刷完 + **有刷课器认得它**.

    content_published:
        True  → 有视频内容, 能刷
        False → 还没内容(老师没放出来), 刷不了
        None  → 判断不了(页面没开/超时) → 不选(宁缺毋滥, 别瞎刷)
    progress:
        0-100 → 「总学习进度」百分比
        None  → 读不到 → 认为要刷(有内容就刷, 顶多多刷一次)

    ⚠️ 「有内容」不等于「刷得了」: 上游 Autovisor 一门课失败就 **break 掉整轮**
    (Autovisor.py: if outcome is CourseOutcome.FAILED: break), 一门失败后面全刷不到。
    所以必须同时满足「有刷课器认得这门课的页面」—— 见 classify_engine。
    """
    picked: list[dict[str, Any]] = []
    for course in report.get("courses") or []:
        if not isinstance(course, dict):
            continue
        if course.get("content_published") is not True:
            continue
        url = (course.get("course_home_url") or "").strip()
        if not url:
            continue
        progress = course.get("progress")
        if isinstance(progress, int) and progress >= threshold:
            continue  # 已刷完
        engine = classify_engine(course)
        if engine is None:
            continue  # 没有刷课器认得它 → 放进去只会让整轮白跑
        picked.append(
            {
                "title": course.get("title") or "",
                "url": url,
                "progress": progress,
                "platform": course.get("platform"),
                "engine": engine,
            }
        )
    # 没读到进度的排前面(更可能还没刷), 其余按进度低→高
    picked.sort(key=lambda c: (c["progress"] is not None, c["progress"] if c["progress"] is not None else 0))
    return picked[:limit]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="生成 Autovisor 的刷课目标")
    parser.add_argument("--report", default="data/report.json")
    parser.add_argument("--config", default="config.ini")
    parser.add_argument("--out", default="data/targets.json")
    parser.add_argument("--threshold", type=int, default=100)
    parser.add_argument("--max", type=int, default=3)
    parser.add_argument("--replace", action="store_true", help="清掉人工维护的 URL")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)

    try:
        with open(args.report, encoding="utf-8") as fh:
            report = json.load(fh)
    except Exception as exc:
        print(f"[targets] 读不到报告 {args.report}: {exc}", file=sys.stderr)
        return 1

    picked = pick_courses(report, args.threshold, args.max)

    config_text = ""
    if os.path.exists(args.config):
        with open(args.config, encoding="utf-8") as fh:
            config_text = fh.read()
    manual = [] if args.replace else read_manual_urls(config_text)

    # config.ini 的 [course-url] 是**上游 Autovisor 专用**的清单:
    # 一门课失败就 break 掉整轮, 所以只能放它认得的课。
    # polymas 的课走自研引擎, 记在 data/targets.json 里(engine 字段), 不塞这里。
    auto_for_autovisor = [c for c in picked if c["engine"] == ENGINE_AUTOVISOR]
    # 人工清单优先(他手动放进去的, 一定有理由), 自动选出的去重后追加
    urls: list[str] = []
    for url in manual + [c["url"] for c in auto_for_autovisor]:
        if url not in urls:
            urls.append(url)

    print(f"[targets] 报告 {args.report}: {len(report.get('courses') or [])} 门课")
    for course in report.get("courses") or []:
        if not isinstance(course, dict):
            continue
        engine = classify_engine(course)
        why = ""
        if course.get("content_published") is not True:
            why = "无内容/判断不了"
        elif engine is None:
            why = "没有刷课器认得它"
        print(
            "  - %-28s content=%-5s progress=%-5s engine=%-9s %s"
            % (
                (course.get("title") or "?")[:28],
                course.get("content_published"),
                course.get("progress"),
                engine or "-",
                why or ("已刷完" if course.get("progress") == 100 else "→ 选中"),
            )
        )
    print(f"[targets] 本轮自动选中 {len(picked)} 门: "
          f"{[(c['title'], c['engine']) for c in picked]}")
    print(f"[targets] 其中给 Autovisor {len(auto_for_autovisor)} 门 → "
          f"config.ini [course-url] 共 {len(urls)} 条")
    if len(picked) > len(auto_for_autovisor):
        print(f"[targets] 另外 {len(picked) - len(auto_for_autovisor)} 门走自研引擎 "
              f"(记在 {args.out}, 不进 config.ini)")

    if args.dry_run:
        print("[targets] dry-run, 未改文件")
        return 0

    new_text = write_course_urls(config_text, urls)
    with open(args.config, "w", encoding="utf-8") as fh:
        fh.write(new_text)
    print(f"[targets] 已更新 {args.config} 的 [course-url] 段")

    targets = {
        "generated_at": report.get("generated_at"),
        "threshold": args.threshold,
        "max": args.max,
        "manual": manual,
        "auto": [c["title"] for c in picked],
        "courses": [
            {
                "title": c["title"],
                "url": c["url"],
                "progress": c["progress"],
                "platform": c["platform"],
                # 由哪个刷课引擎刷: autovisor(上游) / polymas(自研脚本)
                "engine": c["engine"],
                "source": "manual" if c["url"] in manual else "auto",
            }
            for c in picked
        ],
    }
    os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
    with open(args.out, "w", encoding="utf-8") as fh:
        json.dump(targets, fh, ensure_ascii=False, indent=2)
    print(f"[targets] 已写 {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
