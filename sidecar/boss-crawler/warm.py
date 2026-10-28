import time, os, json
import main as m
from main import make_page, logged_in, login_blocked, DATA_DIR, log

SOL = "/data/boss_clicks.json"
try: os.remove(SOL)
except FileNotFoundError: pass

page = make_page()
page.get("https://www.zhipin.com/web/geek/jobs")
time.sleep(3)

for rnd in range(3):
    if not login_blocked(page):
        break
    log(f"verify round {rnd+1}")
    btn = page.ele("text:点击按钮进行验证", timeout=5)
    if btn:
        btn.click(); time.sleep(4)
    page.get_screenshot(path=DATA_DIR, name="boss_captcha.png")
    log("captcha saved; waiting boss_clicks.json (240s)")
    deadline = time.time() + 240
    while time.time() < deadline and not os.path.exists(SOL):
        time.sleep(2)
    if not os.path.exists(SOL):
        log("no clicks, abort"); raise SystemExit(1)
    for (x, y) in json.load(open(SOL))["points"]:
        page.run_cdp("Input.dispatchMouseEvent", type="mousePressed", x=x, y=y, button="left", clickCount=1)
        page.run_cdp("Input.dispatchMouseEvent", type="mouseReleased", x=x, y=y, button="left", clickCount=1)
        time.sleep(0.5)
    os.remove(SOL)
    time.sleep(1)
    ok = page.ele("text:确认", timeout=3)
    if ok: ok.click()
    time.sleep(3)
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(3)
    log(f"after round {rnd+1}: url={page.url}")

log(f"warmup done: blocked={login_blocked(page)} logged_in={logged_in(page)}")
if login_blocked(page) or not logged_in(page):
    log("warmup failed"); page.quit(); raise SystemExit(1)

m.make_page = lambda: page
raise SystemExit(m.main())
