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
    log("captcha screenshot saved, waiting for boss_clicks.json …")
    deadline = time.time() + 300
    while time.time() < deadline:
        if os.path.exists(SOL):
            pts = json.load(open(SOL))["points"]
            log(f"clicking {pts}")
            for (x, y) in pts:
                page.run_cdp("Input.dispatchMouseEvent", type="mousePressed", x=x, y=y, button="left", clickCount=1)
                page.run_cdp("Input.dispatchMouseEvent", type="mouseReleased", x=x, y=y, button="left", clickCount=1)
                time.sleep(0.4)
            time.sleep(1)
            ok = page.ele("text:确认", timeout=3)
            if ok:
                ok.click()
                log("clicked 确认")
            time.sleep(4)
            page.get("https://www.zhipin.com/web/geek/jobs")
            time.sleep(4)
            log(f"after: url={page.url} logged_in={logged_in(page)}")
            page.get_screenshot(path=DATA_DIR, name="boss_after.png")
            break
        time.sleep(2)
    else:
        log("no solution file, gave up")
finally:
    page.quit()
