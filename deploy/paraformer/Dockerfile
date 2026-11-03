# Paraformer-large ASR 服务（sherpa-onnx int8 CPU 版，无 torch）
# SenseVoice 服务的 A/B 对照组：接口一致（whisper.cpp 兼容 /inference），
# ru41 lecture 插件侧只改 asr_base_url 即可切换。
#
# 模型不随镜像下载：先在本目录备好 models/ 再构建（网络与构建解耦）：
#   GH=https://gh-proxy.com/https://github.com/k2-fsa/sherpa-onnx/releases/download
#   curl -sL $GH/asr-models/sherpa-onnx-paraformer-zh-2023-09-14.tar.bz2 | tar -xj \
#     --strip-components=1 -C models sherpa-onnx-paraformer-zh-2023-09-14/model.int8.onnx \
#     sherpa-onnx-paraformer-zh-2023-09-14/tokens.txt
#   curl -sL $GH/punctuation-models/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8.tar.bz2 \
#     | tar -xj --strip-components=1 -C models --wildcards "*/model.int8.onnx" \
#     && mv models/model.int8.onnx models/punct.int8.onnx
#   curl -sL -o models/silero_vad.onnx $GH/asr-models/silero_vad.onnx
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
