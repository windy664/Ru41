import time, os, json
from main import make_page, logged_in, DATA_DIR, log

SOL = "/data/boss_clicks.json"
try: os.remove(SOL)
except FileNotFoundError: pass

page = make_page()
try:
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    btn = page.ele("text:点击按钮进行验证", timeout=5)
    if btn:
        btn.click(); time.sleep(4)
    page.get_screenshot(path=DATA_DIR, name="boss_captcha.png")
    log("captcha saved; waiting solution (300s)")
    deadline = time.time() + 300
    while time.time() < deadline and not os.path.exists(SOL):
        time.sleep(2)
    if not os.path.exists(SOL):
        log("no solution, gave up"); raise SystemExit(1)
    pts = json.load(open(SOL))["points"]
    for (x, y) in pts:
        page.run_cdp("Input.dispatchMouseEvent", type="mousePressed", x=x, y=y, button="left", clickCount=1)
        page.run_cdp("Input.dispatchMouseEvent", type="mouseReleased", x=x, y=y, button="left", clickCount=1)
        time.sleep(0.5)
    time.sleep(1)
    page.get_screenshot(path=DATA_DIR, name="boss_selected.png")
    log("selected screenshot saved; clicking 确认")
    ok = page.ele("text:确认", timeout=3)
    if ok: ok.click()
    time.sleep(4)
    page.get_screenshot(path=DATA_DIR, name="boss_result.png")
    log(f"result url={page.url}")
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    log(f"final url={page.url} logged_in={logged_in(page)}")
    page.get_screenshot(path=DATA_DIR, name="boss_after.png")
finally:
    page.quit()
