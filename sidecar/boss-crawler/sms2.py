import time, os, json
from main import make_page, logged_in, write_status, DATA_DIR, log

PHONE = os.environ["BOSS_PHONE"]  # 触发短信验证码的手机号，经环境变量注入
CODE_FILE = "/data/boss_sms_code.json"
SOL = "/data/boss_clicks.json"
for f in (CODE_FILE, SOL):
    try: os.remove(f)
    except FileNotFoundError: pass

def wait_file(path, secs):
    deadline = time.time() + secs
    while time.time() < deadline and not os.path.exists(path):
        time.sleep(2)
    return os.path.exists(path)

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

    # 第一步：点发送验证码 → 可能出现「点击按钮进行验证」
    page.ele("text:发送验证码", timeout=3).click()
    time.sleep(2.5)
    vb = page.ele("text:点击按钮进行验证", timeout=3)
    if vb:
        log("verify button appeared, clicking")
        vb.click(); time.sleep(4)
        page.get_screenshot(path=DATA_DIR, name="boss_captcha.png")
        log("captcha saved; waiting clicks file (300s)")
        if not wait_file(SOL, 300):
            log("no solution"); raise SystemExit(1)
        for (x, y) in json.load(open(SOL))["points"]:
            page.run_cdp("Input.dispatchMouseEvent", type="mousePressed", x=x, y=y, button="left", clickCount=1)
            page.run_cdp("Input.dispatchMouseEvent", type="mouseReleased", x=x, y=y, button="left", clickCount=1)
            time.sleep(0.5)
        time.sleep(1)
        page.get_screenshot(path=DATA_DIR, name="boss_selected.png")
        ok = page.ele("text:确认", timeout=3)
        if ok: ok.click()
        time.sleep(3)
        page.get_screenshot(path=DATA_DIR, name="boss_result.png")
        log(f"after captcha url={page.url}")
        # 有的流程过验证码后要再点一次发送
        again = page.ele("text:发送验证码", timeout=2) or page.ele("text:重新发送", timeout=2)
        st = page.ele("css:.btn-sms", timeout=1)
        if again:
            again.click(); log("re-clicked send code")
            time.sleep(2)
    page.get_screenshot(path=DATA_DIR, name="boss_sms_sent.png")
    log("waiting sms code file (600s)…")
    if not wait_file(CODE_FILE, 600):
        log("no code"); raise SystemExit(1)
    code = json.load(open(CODE_FILE))["code"]
    ci = page.ele("css:input[placeholder*='验证码']", timeout=5)
    ci.clear(); ci.input(code); time.sleep(0.5)
    sub = page.ele("text:登录/注册", timeout=3) or page.ele("text:登录", timeout=2)
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
