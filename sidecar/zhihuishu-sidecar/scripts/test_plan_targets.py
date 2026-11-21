"""容器内/宿主自测: 刷课目标生成器(plan_targets.py).

覆盖:
1. 选课规则 —— 有内容(content_published=True) 且 没刷完(progress<100) 才选;
   没内容的、判断不了的、已刷完的都不选; progress 读不到时按「要刷」处理。
2. 改写 config.ini 时**只动 [course-url] 段的选项行** —— 注释(含防风控说明)、
   其余段落必须原样保留(configparser 重写会把注释全丢掉)。
3. 人工维护的 URL 保留并排在自动选出的前面, 且与自动选出去重。

跑法:
    python3 -u scripts/test_plan_targets.py
"""
import json
import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from plan_targets import (  # noqa: E402
    classify_engine,
    pick_courses,
    read_manual_urls,
    write_course_urls,
)

# ---- 选课规则 ---------------------------------------------------------------

# 上游 Autovisor 认得的课程页(AI 课) 与 polymas 平台课程页
WISDOM = "https://ai-smart-course-student-pro.zhihuishu.com/singleCourse/knowledgeStudy/1/2"
POLY = "https://hike-teaching-center.polymas.com/agent-course-full/xxx/stu/course-home"

REPORT = {
    "generated_at": "2026-11-21 00:30:00",
    "login_ok": True,
    "courses": [
        # 有内容 + 进度 40% + 上游认得 → 要刷(autovisor)
        {"title": "程序设计基础", "course_home_url": WISDOM + "a", "content_published": True, "progress": 40, "catalog": "wisdom"},
        # 有内容 + 已刷完 100% → 不刷
        {"title": "军事理论", "course_home_url": WISDOM + "b", "content_published": True, "progress": 100, "catalog": "wisdom"},
        # 有内容但读不到进度 → 当作要刷(顶多多刷一次)
        {"title": "高等数学I(理)", "course_home_url": WISDOM + "c", "content_published": True, "progress": None, "catalog": "wisdom"},
        # 还没放内容 → 刷不了
        {"title": "国家安全教育", "course_home_url": POLY + "a", "content_published": False, "progress": 0},
        # 判断不了(页面没开) → 宁缺毋滥, 不选
        {"title": "形势与政策", "course_home_url": POLY + "b", "content_published": None, "progress": None},
        # 没 URL → 选不了
        {"title": "无链接课", "course_home_url": "", "content_published": True, "progress": 10},
        # polymas 课: 上游认不得(catalog=None) → 自研引擎
        {"title": "思想道德与法治", "course_home_url": POLY + "c", "content_published": True, "progress": 0, "catalog": None},
        # 谁都认不得的课 → 不选(塞进清单只会让整轮白跑)
        {"title": "来路不明的课", "course_home_url": "https://unknown.example.com/x", "content_published": True, "progress": 0},
    ],
}

# ---- 引擎分流 ---------------------------------------------------------------

assert classify_engine(REPORT["courses"][0]) == "autovisor"
assert classify_engine(REPORT["courses"][6]) == "polymas"
assert classify_engine(REPORT["courses"][7]) is None
print("  ok  引擎分流: 上游认得的→autovisor, polymas→自研, 谁都不认→不选")

picked = pick_courses(REPORT, threshold=100, limit=10)
titles = [c["title"] for c in picked]
assert "程序设计基础" in titles, titles
assert "高等数学I(理)" in titles, titles
assert "思想道德与法治" in titles, titles
assert "军事理论" not in titles, titles
assert "国家安全教育" not in titles, titles
assert "形势与政策" not in titles, titles
assert "无链接课" not in titles, titles
assert "来路不明的课" not in titles, titles
print("  ok  选课规则: 有内容+没刷完+有引擎认得 才选")

# 阈值: progress>=阈值 的不选; progress 读不到的仍算「要刷」(顶多多刷一次)
picked90 = pick_courses(REPORT, threshold=90, limit=10)
titles90 = [c["title"] for c in picked90]
assert "军事理论" not in titles90, titles90  # progress=100 >= 90 → 不选
assert "程序设计基础" in titles90, titles90  # progress=40 < 90 → 选
assert "高等数学I(理)" in titles90, titles90  # progress 读不到 → 当作要刷
print("  ok  阈值: progress>=阈值 的不选, 读不到的仍算要刷")

# 每晚上限
picked1 = pick_courses(REPORT, threshold=100, limit=1)
assert len(picked1) == 1, picked1
print("  ok  --max 上限生效")

# progress 读不到的排前面(更可能还没刷)
assert picked[0]["progress"] is None, picked
print("  ok  progress 读不到的排前面")

# ---- 只改 [course-url] 段 ---------------------------------------------------

CONFIG = """# 智慧树配置(手改, 别让工具覆盖注释)
[user-account]
username = 1xxxxxxxxxx
password = xxxxxxxxxx

[browser-option]
driver = Chrome
EXE_PATH = /usr/local/bin/zhihuishu-chromium

[script-option]
enableAutoCaptcha = True
# 是否自动隐藏浏览器窗口(默认:False) —— Windows 专用, Linux 容器无效
enableHideWindow = False

[course-option]
# 限制每门课程刷课时长/min (不限制:0) —— 不 24h 挂机, 防人机验证
limitMaxTime = 30
limitSpeed = 1.0

[course-url]
URL1=https://manual.example/keepme
"""

manual = read_manual_urls(CONFIG)
assert manual == ["https://manual.example/keepme"], manual
print("  ok  读出人工维护的 URL")

# 裸首页(登录测试留下的占位)不算课程 —— Autovisor 拿到会 break 掉整轮
PLACEHOLDER = """[course-url]
;URL1 = https://www.zhihuishu.com  ← 临时占位，仅用于登录测试
URL1 = https://www.zhihuishu.com
URL2 = https://www.zhihuishu.com/
URL3 = https://www.zhihuishu.com/course/realone
"""
manual_ph = read_manual_urls(PLACEHOLDER)
assert manual_ph == ["https://www.zhihuishu.com/course/realone"], manual_ph
print("  ok  裸首页占位被剔除(否则 Autovisor 会 break 掉整轮)")

new_text = write_course_urls(CONFIG, ["https://manual.example/keepme", "https://x/1", "https://x/3"])
assert "limitMaxTime = 30" in new_text
assert "不 24h 挂机, 防人机验证" in new_text, "注释不能丢"
assert "enableHideWindow = False" in new_text
assert "password = xxxxxxxxxx" in new_text
assert "URL1=https://manual.example/keepme" in new_text
assert "URL2=https://x/1" in new_text
assert "URL3=https://x/3" in new_text
assert new_text.count("[course-url]") == 1
print("  ok  只改 [course-url] 段, 注释与其余段落原样保留")

# 原来没有 [course-url] 段 → 追加一段
no_section = "[user-account]\nusername = 1\n"
added = write_course_urls(no_section, ["https://x/9"])
assert "[course-url]" in added
assert "URL1=https://x/9" in added
assert read_manual_urls(added) == ["https://x/9"]
print("  ok  缺 [course-url] 段时自动追加")

# 幂等: 再写一次同样的结果不变
again = write_course_urls(new_text, ["https://manual.example/keepme", "https://x/1", "https://x/3"])
assert again == new_text, "重写应幂等"
print("  ok  重写幂等")

# ---- 端到端: 真文件 -------------------------------------------------------

with tempfile.TemporaryDirectory() as tmp:
    report_path = os.path.join(tmp, "report.json")
    config_path = os.path.join(tmp, "config.ini")
    out_path = os.path.join(tmp, "targets.json")
    with open(report_path, "w", encoding="utf-8") as fh:
        json.dump(REPORT, fh, ensure_ascii=False)
    with open(config_path, "w", encoding="utf-8") as fh:
        fh.write(CONFIG)

    from plan_targets import main

    rc = main(
        [
            "--report", report_path,
            "--config", config_path,
            "--out", out_path,
            "--max", "3",
        ]
    )
    assert rc == 0, rc
    with open(config_path, encoding="utf-8") as fh:
        written = fh.read()
    assert "不 24h 挂机, 防人机验证" in written
    urls = read_manual_urls(written)
    assert urls[0] == "https://manual.example/keepme", urls
    # config.ini 只装上游 Autovisor 认得的课(polymas 的走自研引擎, 不塞这里)
    assert all("polymas" not in u for u in urls), urls
    assert len(urls) == 3, urls  # 1 人工 + 2 上游能刷的
    with open(out_path, encoding="utf-8") as fh:
        targets = json.load(fh)
    assert targets["auto"], targets
    assert targets["manual"] == ["https://manual.example/keepme"], targets
    engines = {c["title"]: c["engine"] for c in targets["courses"]}
    assert engines.get("思想道德与法治") == "polymas", engines
    assert engines.get("程序设计基础") == "autovisor", engines
    print("  ok  端到端: 报告 → config.ini [course-url](仅上游能刷的) + targets.json(带引擎)")

print("全部通过")
