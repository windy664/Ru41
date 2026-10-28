# 智慧树水课 sidecar —— 重依赖独立镜像，不进 Ru41 主镜像（deploy/README.md §5 约定）
FROM mcr.microsoft.com/playwright/python:v1.49.0-jammy

# 虚拟显示：有头模式跑，规避 headless 检测与"最小化不涨进度"
RUN apt-get update \
 && apt-get install -y --no-install-recommends xvfb \
 && rm -rf /var/lib/apt/lists/*

# 国内网络：PyPI 走清华镜像，Playwright 浏览器走 npmmirror，超时放宽
ENV PIP_INDEX_URL=https://pypi.tuna.tsinghua.edu.cn/simple \
    PIP_DEFAULT_TIMEOUT=120 \
    PLAYWRIGHT_DOWNLOAD_HOST=https://cdn.npmmirror.com/binaries/playwright

WORKDIR /app
COPY Autovisor/ .

# captcha extra = numpy + opencv（enableAutoCaptcha 过滑块要用）
RUN pip install --no-cache-dir -U pip \
 && pip install --no-cache-dir --retries 5 '.[captcha]'

# 对齐 pip 解析出的 playwright 版本与 /ms-playwright 内浏览器 revision
RUN playwright install chromium

# 新版 Chrome for Testing 布局是 chrome-linux64/，旧版是 chrome-linux/，两者都兼容；
# 版本排序取最新（镜像自带的旧浏览器目录也匹配 glob）
RUN chrome="$(ls -d /ms-playwright/chromium-*/chrome-linux*/chrome | sort -V | tail -n1)" \
 && ln -sf "$chrome" /usr/local/bin/zhihuishu-chromium

# xvfb-run 的 SIGUSR1 就绪握手存在竞态(信号早于 wait 到达即永久卡死)，显式拉起 Xvfb 规避
CMD ["sh", "-c", "Xvfb :99 -screen 0 1600x900x24 -nolisten tcp & XPID=$!; sleep 1.5; DISPLAY=:99 python Autovisor.py; RC=$?; kill $XPID 2>/dev/null; exit $RC"]
