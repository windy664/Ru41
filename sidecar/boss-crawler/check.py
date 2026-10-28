import time, os
from main import make_page, logged_in, login_blocked, DATA_DIR, log
page = make_page()
try:
    page.get("https://www.zhipin.com/web/geek/jobs")
    time.sleep(4)
    print("URL:", page.url)
    print("TITLE:", page.title)
    print("LOGGED_IN:", logged_in(page))
    print("BLOCKED:", login_blocked(page))
    page.get_screenshot(path=DATA_DIR, name="boss_check.png")
finally:
    page.quit()
