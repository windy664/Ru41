import time, os, json, random
from main import make_page, logged_in, write_status, DATA_DIR, log

PHONE = os.environ["BOSS_PHONE"]  # 触发短信验证码的手机号，经环境变量注入
CODE_FILE = "/data/boss_sms_code.json"

def wait_file(path, secs):
    deadline = time.time() + secs
    while time.time() < deadline and not os.path.exists(path):
        time.sleep(2)
    return os.path.exists(path)

def drag(page, x0, y0, dx):
    page.run_cdp("Input.dispatchMouseEvent", type="mousePressed", x=x0, y=y0, button="left", clickCount=1)
    steps = max(12, int(dx / 6))
    for i in range(steps):
        t = (i + 1) / steps
        x = x0 + dx * (1 - (1 - t) ** 2)
        page.run_cdp("Input.dispatchMouseEvent", type="mouseMoved", x=int(x), y=y0 + random.randint(-1, 1))
        time.sleep(random.uniform(0.012, 0.035))
    time.sleep(random.uniform(0.3, 0.6))
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

    for rnd in range(1, 5):
        drag_file = f"/data/boss_drag{rnd}.json"
        try: os.remove(drag_file)
        except FileNotFoundError: pass
        # 先拖 50px 让拼图露出来（第一轮）；后续轮拼图已可见
        if rnd == 1:
            drag(page, 602, 450, 50)
            time.sleep(1.2)
        page.get_screenshot(path=DATA_DIR, name=f"boss_round{rnd}.png")
        log(f"round {rnd} screenshot saved, waiting {drag_file} (180s)")
        if not wait_file(drag_file, 180):
            log("no drag file, abort"); raise SystemExit(1)
        spec = json.load(open(drag_file))
        drag(page, spec["x"], spec.get("y", 450), spec["dx"])
        time.sleep(2.5)
        page.get_screenshot(path=DATA_DIR, name=f"boss_round{rnd}_after.png")
        # 成功标志：滑块行消失（找不到"向右拖动"文本）或出现倒计时
        if not page.ele("text:向右拖动", timeout=2):
            log("slider gone, captcha likely passed")
            break
        log(f"round {rnd} failed, slider still there")
    send = page.ele("text:发送验证码", timeout=2) or page.ele("text:重新发送", timeout=2)
    if send:
        send.click(); log("clicked send"); time.sleep(2)
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
