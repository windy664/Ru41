FROM python:3.12-slim

ENV DEBIAN_FRONTEND=noninteractive
# 国内构建：apt/pip 换镜像源（deb.debian.org / pypi.org 直连太慢）。
RUN sed -i 's|deb.debian.org|mirrors.aliyun.com|g' /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends chromium fonts-noto-cjk \
    && rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir -i https://mirrors.aliyun.com/pypi/simple/ DrissionPage

WORKDIR /app
COPY main.py login.py ./

ENV BOSS_DATA_DIR=/data \
    BOSS_BROWSER_PATH=/usr/bin/chromium \
    BOSS_HEADLESS=true \
    PYTHONUNBUFFERED=1

VOLUME /data
CMD ["python", "main.py"]
