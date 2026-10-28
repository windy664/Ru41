import time, os, json, math, random
from main import make_page, logged_in, write_status, DATA_DIR, log

PHONE = os.environ["BOSS_PHONE"]  # 触发短信验证码的手机号，经环境变量注入
CODE_FILE = "/data/boss_sms_code.json"
DRAG_FILE = "/data/boss_drag.json"
for f in (CODE_FILE, DRAG_FILE):
    try: os.remove(f)
    except FileNotFoundError: pass

def wait_file(path, secs):
    deadline = time.time() + secs
    while time.time() < deadline and not os.path.exists(path):
        time.sleep(2)
    return os.path.exists(path)

def drag(page, x0, y0, dx):
    """拟人拖动：先快后慢带过冲回退。"""
    page.run_cdp("Input.dispatchMouseEvent", type="mousePressed", x=x0, y=y0, button="left", clickCount=1)
    steps = max(10, int(dx / 8))
    overshoot = dx + random.randint(3, 8)
    for i in range(steps):
        t = i / steps
        x = x0 + overshoot * (1 - (1 - t) ** 2)  # ease-out
        page.run_cdp("Input.dispatchMouseEvent", type="mouseMoved", x=int(x), y=y0 + random.randint(-1, 1))
        time.sleep(random.uniform(0.015, 0.04))
    for i in range(4):  # 回退到目标
        x = x0 + overshoot - (overshoot - dx) * (i + 1) / 4
        page.run_cdp("Input.dispatchMouseEvent", type="mouseMoved", x=int(x), y=y0)
        time.sleep(0.03)
    time.sleep(random.uniform(0.2, 0.5))
    page.run_cdp("Input.dispatchMouseEvent", type="mouseReleased", x=x0 + dx, y=y0, button="left", clickCount=1)

page = make_page()
try:
    page.get("https://www.zhipin.com/web/user/")
    time.sleep(3)
    tab = page.ele("text:验证码登录", timeout=3)
    if tab: tab.click(by_js=True); time.sleep(1)
    inp = page.ele("css:input[placeholder*='手机号']", timeout=5)
    inp.clear(); inp.input(PHONE); time.sleep(0.5)
    cb = page.ele("css:input[type=checkbox]", timeout=2)
    if cb and not cb.attr("checked"): cb.click(by_js=True)
    page.ele("text:发送验证码", timeout=3).click()
    time.sleep(2.5)
    # 滑块行：先拖一小段让拼图露出来
    drag(page, 602, 450, 60)
    time.sleep(1.5)
    page.get_screenshot(path=DATA_DIR, name="boss_slider.png")
    log("slider revealed; waiting boss_drag.json {dx_total} (300s)")
    if not wait_file(DRAG_FILE, 300):
        log("no drag file"); raise SystemExit(1)
    dx = json.load(open(DRAG_FILE))["dx_total"] - 60
    if dx > 5:
        drag(page, 662, 450, dx)
    time.sleep(2.5)
    page.get_screenshot(path=DATA_DIR, name="boss_after_drag.png")
    log(f"after drag url={page.url}")
    send = page.ele("text:发送验证码", timeout=2) or page.ele("text:重新发送", timeout=2)
    if send:
        send.click(); log("clicked send again"); time.sleep(2)
    page.get_screenshot(path=DATA_DIR, name="boss_sms_sent.png")
    log("waiting sms code (600s)…")
    if not wait_file(CODE_FILE, 600):
        log("no code"); raise SystemExit(1)
    code = json.load(open(CODE_FILE))["code"]
    ci = page.ele("css:input[placeholder*='验证码']", timeout=5)
    ci.clear(); ci.input(code); time.sleep(0.5)
    sub = page.ele("text:登录/注册", timeout=3)
    if sub: sub.click()
    time.sleep(5)
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    ok = logged_in(page)
    log(f"final url={page.url} logged_in={ok}")
    write_status(ok)
    page.get_screenshot(path=DATA_DIR, name="boss_after.png")
finally:
    page.quit()
