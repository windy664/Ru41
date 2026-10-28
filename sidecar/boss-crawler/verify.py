import time
from main import make_page, DATA_DIR, log

page = make_page()
try:
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    log(f"url={page.url}")
    # 点「点击按钮进行验证」
    btn = page.ele("text:点击按钮进行验证", timeout=5)
    if btn:
        btn.click()
        log("clicked verify button")
        time.sleep(3)
    page.get_screenshot(path=DATA_DIR, name="boss_verify1.png")
    # 看有没有滑块
    slider = page.ele("css:.geetest_slider_button", timeout=3) or page.ele("css:.slide-btn", timeout=1) or page.ele("xpath://div[contains(@class,'slider')]", timeout=1)
    log(f"slider element: {slider}")
    # iframe 情况
    iframes = page.eles("tag:iframe")
    log(f"iframes: {len(iframes)}")
    for i, f in enumerate(iframes):
        log(f"iframe{i} src={f.attr('src')}")
finally:
    page.quit()
