import time
from main import make_page, login_blocked, log

page = make_page()
try:
    page.listen.start()  # 全部请求
    page.get("https://www.zhipin.com/web/geek/jobs?city=101280100&query=AI%20Agent%20%E5%AE%9E%E4%B9%A0")
    time.sleep(8)
    seen = set()
    while True:
        r = page.listen.wait(timeout=2)
        if not r:
            break
        u = r.url
        if any(k in u for k in ("wapi", "json", "search", "job")) and u not in seen:
            seen.add(u)
            log(f"REQ {u[:130]}")
    page.listen.stop()
    log(f"blocked={login_blocked(page)} title={page.title}")
    page.get_screenshot(path="/data", name="boss_probe.png")
finally:
    page.quit()
