FROM python:3.12-slim

ENV DEBIAN_FRONTEND=noninteractive
# chromium + 中文字体（截图里的二维码和页面文字要能渲染）。
RUN apt-get update \
    && apt-get install -y --no-install-recommends chromium fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir DrissionPage

WORKDIR /app
COPY main.py login.py ./

ENV BOSS_DATA_DIR=/data \
    BOSS_BROWSER_PATH=/usr/bin/chromium \
    BOSS_HEADLESS=true \
    PYTHONUNBUFFERED=1

VOLUME /data
CMD ["python", "main.py"]
