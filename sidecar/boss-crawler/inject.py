import time, json
from main import make_page, logged_in, write_status, DATA_DIR, log

cookies = json.load(open("/data/zhipin_cookies.json"))
page = make_page()
try:
    page.get("https://www.zhipin.com/")
    time.sleep(2)
    ck = []
    for c in cookies:
        ck.append({
            "name": c["name"], "value": c["value"],
            "domain": c["host"], "path": c["path"] or "/",
            "secure": c["secure"], "httpOnly": c["httponly"],
        })
    page.set.cookies(ck)
    log(f"injected {len(ck)} cookies")
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    ok = logged_in(page)
    log(f"url={page.url} logged_in={ok}")
    page.get_screenshot(path=DATA_DIR, name="boss_after.png")
    write_status(ok)
finally:
    page.quit()
