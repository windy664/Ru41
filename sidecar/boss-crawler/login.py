#!/usr/bin/env python3
"""BOSS 直聘登录辅助：未登录时把登录页二维码截图写到 /data/boss_login_needed.png，
然后轮询等待扫码完成（默认 10 分钟），成功后登录态落进持久化 profile。

用法：docker run --rm -v /home/windy2/boss-crawler-data:/data boss-crawler:local python login.py
把 PNG 从数据目录取出来给用户扫码即可；扫完本脚本自动检测到并退出。
"""

import os
import sys
import time

from main import LOGIN_SHOT, PROFILE_DIR, logged_in, make_page, write_status, log

WAIT_SECS = int(os.environ.get("BOSS_LOGIN_WAIT_SECS", "600"))


def main():
    os.makedirs(PROFILE_DIR, exist_ok=True)
    page = make_page()
    try:
        page.get("https://www.zhipin.com/web/geek/jobs")
        time.sleep(2)
        if logged_in(page):
            log("已登录，无需扫码")
            write_status(True)
            return 0
        log("未登录，打开登录页并截图二维码…")
        page.get("https://www.zhipin.com/web/user/")
        time.sleep(3)
        page.get_screenshot(path=os.path.dirname(LOGIN_SHOT), name=os.path.basename(LOGIN_SHOT))
        write_status(False, note="等待扫码")
        log(f"二维码已存 {LOGIN_SHOT}，等待扫码（最长 {WAIT_SECS}s）…")

        deadline = time.time() + WAIT_SECS
        while time.time() < deadline:
            time.sleep(5)
            try:
                page.get("https://www.zhipin.com/web/geek/jobs")
                time.sleep(1)
                if logged_in(page):
                    log("扫码成功，登录态已持久化")
                    write_status(True)
                    return 0
            except Exception as e:
                log(f"复检异常: {e}")
        log("等待超时，未检测到登录")
        write_status(False, note="扫码超时")
        return 1
    finally:
        page.quit()


if __name__ == "__main__":
    sys.exit(main())
