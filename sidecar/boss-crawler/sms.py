import time, os, json
from main import make_page, logged_in, DATA_DIR, log

PHONE = os.environ["BOSS_PHONE"]  # 触发短信验证码的手机号，经环境变量注入
CODE_FILE = "/data/boss_sms_code.json"
try: os.remove(CODE_FILE)
except FileNotFoundError: pass

page = make_page()
try:
    page.get("https://www.zhipin.com/web/user/")
    time.sleep(3)
    page.get_screenshot(path=DATA_DIR, name="boss_sms_page.png")
    # 确保在「验证码登录/注册」标签（默认就是，但保险点一下）
    tab = page.ele("text:验证码登录", timeout=3)
    if tab:
        tab.click(by_js=True); time.sleep(1)
    # 填手机号
    inp = page.ele("css:input[placeholder*='手机号']", timeout=5) or page.ele("css:input[type='tel']", timeout=2)
    log(f"phone input: {bool(inp)}")
    inp.clear(); inp.input(PHONE)
    time.sleep(0.5)
    # 同意协议勾选框（如有）
    cb = page.ele("css:.login-agreement input[type=checkbox]", timeout=2) or page.ele("css:input[type=checkbox]", timeout=2)
    if cb and not cb.attr("checked"):
        cb.click(by_js=True); log("checked agreement")
    # 点获取验证码
    btn = page.ele("text:发送验证码", timeout=3)
    if btn:
        btn.click(); log("clicked 获取验证码")
    time.sleep(3)
    page.get_screenshot(path=DATA_DIR, name="boss_sms_sent.png")
    log("waiting sms code file (600s)…")
    deadline = time.time() + 600
    while time.time() < deadline and not os.path.exists(CODE_FILE):
        time.sleep(3)
    if not os.path.exists(CODE_FILE):
        log("no code received, gave up"); raise SystemExit(1)
    code = json.load(open(CODE_FILE))["code"]
    log(f"got code {code}, filling")
    code_inp = page.ele("css:input[placeholder*='验证码']", timeout=5)
    code_inp.clear(); code_inp.input(code)
    time.sleep(0.5)
    submit = page.ele("text:登录", timeout=3) or page.ele("css:.btn-login", timeout=2)
    if submit: submit.click()
    time.sleep(4)
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    log(f"final url={page.url} logged_in={logged_in(page)}")
    page.get_screenshot(path=DATA_DIR, name="boss_after.png")
    from main import write_status
    write_status(logged_in(page))
finally:
    page.quit()
