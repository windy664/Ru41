#!/usr/bin/env python3
"""BOSS 直聘实习岗爬取 sidecar（ru41 岗位总览第二数据源）。

DrissionPage 驱动持久化 profile 的 Chromium，按 关键词 × 城市 搜索，
列表截 zpgeek/search/joblist.json、详情截 wapi/zpgeek/job/detail.json。
结果合并写 /data/boss_jobs.json（encryptJobId 去重，14 天没再出现的剔除）。

未登录或遇极验验证码：截图 /data/boss_login_needed.png + 写
/data/boss_status.json {"login_ok": false} 后优雅退出，等人工扫码
（先跑 python login.py 把二维码落盘）。

节奏写死偏保守：关键词间 3-8s、详情间 3-5s、单轮详情上限 60（环境变量可调）。
"""

import json
import os
import random
import re
import sys
import time
import urllib.parse
from datetime import date, datetime, timedelta

from DrissionPage import ChromiumOptions, WebPage

DATA_DIR = os.environ.get("BOSS_DATA_DIR", "/data")
PROFILE_DIR = os.environ.get("BOSS_PROFILE_DIR", os.path.join(DATA_DIR, "boss-profile"))
JOBS_PATH = os.path.join(DATA_DIR, "boss_jobs.json")
STATUS_PATH = os.path.join(DATA_DIR, "boss_status.json")
LOGIN_SHOT = os.path.join(DATA_DIR, "boss_login_needed.png")

KEYWORDS = [
    k.strip()
    for k in os.environ.get("BOSS_KEYWORDS", "AI Agent 实习,大模型 实习,后端 实习,算法 实习").split(",")
    if k.strip()
]
# BOSS 城市码（hot 城市表）。未收录的名字用 城市=码 语法显式给，如 "杭州=101210100"。
CITY_CODES = {"广州": "101280100", "深圳": "101280600", "全国": "100010000"}


def cities():
    out = []
    for item in os.environ.get("BOSS_CITIES", "广州,深圳").split(","):
        item = item.strip()
        if not item:
            continue
        if "=" in item:
            name, code = item.split("=", 1)
            out.append((name.strip(), code.strip()))
        elif item in CITY_CODES:
            out.append((item, CITY_CODES[item]))
        else:
            print(f"[warn] 未知城市 {item}，跳过（可用 城市=码 语法指定）")
    return out


MAX_DETAILS = int(os.environ.get("BOSS_MAX_DETAILS", "60"))
KEEP_DAYS = 14


def log(msg):
    print(f"[{datetime.now():%H:%M:%S}] {msg}", flush=True)


def make_page():
    co = ChromiumOptions()
    co.set_browser_path(os.environ.get("BOSS_BROWSER_PATH", "/usr/bin/chromium"))
    co.set_user_data_path(PROFILE_DIR)
    co.headless(os.environ.get("BOSS_HEADLESS", "true").lower() != "false")
    co.set_argument("--no-sandbox")
    co.set_argument("--disable-dev-shm-usage")
    co.set_argument("--disable-blink-features=AutomationControlled")
    co.set_argument("--window-size=1280,900")
    co.auto_port()
    return WebPage(chromium_options=co)


def login_blocked(page):
    """URL 含 login/security-check，或页面出现极验面板 → 需要人工介入。"""
    url = (page.url or "").lower()
    if "login" in url or "security-check" in url:
        return True
    if page.eles("css:.geetest_panel", timeout=2):
        return True
    return False


def logged_in(page):
    """头像/推荐入口元素在 = 已登录；登录按钮在 = 未登录。"""
    try:
        if page.eles('xpath://a[@href="https://www.zhipin.com/web/geek/recommend"]//img', timeout=2):
            return True
    except Exception:
        pass
    try:
        if page.eles('xpath://a[@class="btn btn-outline header-login-btn"]', timeout=2):
            return False
    except Exception:
        pass
    return not login_blocked(page)


def write_status(ok, jobs=0, note=""):
    payload = {"login_ok": ok, "jobs": jobs, "updated_at": datetime.now().isoformat(timespec="seconds")}
    if note:
        payload["note"] = note
    with open(STATUS_PATH, "w", encoding="utf-8") as f:
        json.dump(payload, f, ensure_ascii=False, indent=2)


def open_login_qr(page):
    """打开登录页并切到扫码登录标签（默认是短信登录），让截图里有二维码。"""
    page.get("https://www.zhipin.com/web/user/")
    time.sleep(3)
    try:
        tab = page.ele("text:扫码登录", timeout=3)
        if tab:
            tab.click(by_js=True)
            time.sleep(2)
    except Exception:
        pass


def bail_for_login(page, note):
    log(f"[!] {note}，截图存 {LOGIN_SHOT}，等人扫码后重跑")
    try:
        page.get_screenshot(path=DATA_DIR, name="boss_login_needed.png")
    except Exception as e:
        log(f"[warn] 截图失败: {e}")
    write_status(False, note=note)


def load_jobs():
    try:
        with open(JOBS_PATH, encoding="utf-8") as f:
            data = json.load(f)
        return data if isinstance(data, dict) and "jobs" in data else {"jobs": []}
    except (OSError, ValueError):
        return {"jobs": []}


def save_jobs(store):
    store["updated_at"] = datetime.now().isoformat(timespec="seconds")
    tmp = JOBS_PATH + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(store, f, ensure_ascii=False, indent=2)
    os.replace(tmp, JOBS_PATH)


def fetch_detail(page, encrypt_job_id):
    """进详情页截 detail.json，拿 JD 全文；失败返回 ''。"""
    url = f"https://www.zhipin.com/job_detail/{encrypt_job_id}.html"
    try:
        page.listen.start("job/detail.json")
        page.get(url)
        r = page.listen.wait(timeout=10)
        page.listen.stop()
        if not r:
            return ""
        zp = r.response.body.get("zpData", {})
        info = zp.get("jobInfo", {})
        desc = info.get("postDescription", "") or ""
        for key in ("job_description", "job_require", "jobRequire", "jobDescription"):
            val = info.get(key, "")
            if val:
                desc += "\n" + val
        return desc.strip()
    except Exception as e:
        log(f"  [warn] 详情失败 {encrypt_job_id}: {e}")
        try:
            page.listen.stop()
        except Exception:
            pass
        return ""


def main():
    os.makedirs(DATA_DIR, exist_ok=True)
    page = make_page()
    try:
        log("打开 BOSS 首页检测登录态…")
        page.get("https://www.zhipin.com/web/geek/jobs")
        time.sleep(2)
        if login_blocked(page):
            bail_for_login(page, "命中登录页或验证码")
            return 0
        if not logged_in(page):
            # 先跳登录页切到扫码标签再截图，让 PNG 里带可扫的二维码。
            try:
                open_login_qr(page)
            except Exception:
                pass
            bail_for_login(page, "未登录")
            return 0

        today = date.today().isoformat()
        store = load_jobs()
        jobs = {j["encryptJobId"]: j for j in store["jobs"] if j.get("encryptJobId")}

        details_done = 0
        for keyword in KEYWORDS:
            for city_name, city_code in cities():
                if details_done >= MAX_DETAILS:
                    break
                query = urllib.parse.quote(keyword)
                url = f"https://www.zhipin.com/web/geek/jobs?city={city_code}&query={query}"
                log(f"搜索 {keyword} @ {city_name}")
                page.listen.start("zpgeek/search/joblist.json")
                page.get(url)
                r = page.listen.wait(timeout=10)
                page.listen.stop()
                if login_blocked(page):
                    bail_for_login(page, "搜索中被风控/登出")
                    save_jobs(store)
                    return 0
                if not r:
                    log("  无响应，跳过")
                    time.sleep(random.uniform(3, 8))
                    continue
                job_list = r.response.body.get("zpData", {}).get("jobList", []) or []
                log(f"  列表 {len(job_list)} 条")
                new_count = 0
                for j in job_list:
                    jid = j.get("encryptJobId")
                    if not jid:
                        continue
                    entry = jobs.get(jid)
                    if entry:
                        entry["last_seen"] = today
                        continue
                    entry = {
                        "encryptJobId": jid,
                        "job_name": j.get("jobName", ""),
                        "company": j.get("brandName", ""),
                        "city": j.get("cityName", "") or city_name,
                        "salary": j.get("salaryDesc", ""),
                        "experience": j.get("jobExperience", ""),
                        "degree": j.get("jobDegree", ""),
                        "skills": j.get("skills", []) or [],
                        "link": f"https://www.zhipin.com/job_detail/{jid}.html",
                        "jd": "",
                        "first_seen": today,
                        "last_seen": today,
                    }
                    if details_done < MAX_DETAILS:
                        entry["jd"] = fetch_detail(page, jid)
                        details_done += 1
                        if login_blocked(page):
                            bail_for_login(page, "详情页被风控/登出")
                            jobs[jid] = entry
                            store["jobs"] = list(jobs.values())
                            save_jobs(store)
                            return 0
                        time.sleep(random.uniform(3, 5))
                    jobs[jid] = entry
                    new_count += 1
                log(f"  新增 {new_count} 条")
                time.sleep(random.uniform(3, 8))

        # 剔除超过 KEEP_DAYS 没再出现的岗位（关闭或沉底）。
        cutoff = (date.today() - timedelta(days=KEEP_DAYS)).isoformat()
        kept = [j for j in jobs.values() if j.get("last_seen", "") >= cutoff]
        store["jobs"] = sorted(kept, key=lambda j: j.get("first_seen", ""), reverse=True)
        save_jobs(store)
        write_status(True, jobs=len(kept))
        log(f"完成：库内 {len(kept)} 条（本轮详情 {details_done} 条）")
        return 0
    finally:
        page.quit()


if __name__ == "__main__":
    sys.exit(main())
