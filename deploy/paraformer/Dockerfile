# Paraformer-large ASR 服务（sherpa-onnx int8 CPU 版，无 torch）
# SenseVoice 服务的 A/B 对照组：接口一致（whisper.cpp 兼容 /inference），
# ru41 lecture 插件侧只改 asr_base_url 即可切换。
# 模型不随镜像下载：宿主机预先拉好放 models/，构建只 COPY（网络与构建解耦）。
FROM python:3.11-slim

RUN sed -i 's|deb.debian.org|mirrors.tuna.tsinghua.edu.cn|g' /etc/apt/sources.list.d/debian.sources && \
    apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg curl && \
    rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir -i https://pypi.tuna.tsinghua.edu.cn/simple \
        sherpa-onnx soundfile fastapi "uvicorn[standard]" python-multipart

COPY models/ /models/

ENV OMP_NUM_THREADS=2 \
    MODEL_DIR=/models \
    PYTHONUNBUFFERED=1

WORKDIR /app
COPY server.py /app/

EXPOSE 50000
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:50000/', timeout=3)" || exit 1

CMD ["uvicorn", "server:app", "--host", "0.0.0.0", "--port", "50000"]
