# SenseVoice ASR 服务（sherpa-onnx int8 CPU 版，无 torch）
# 为 ru41 lecture 插件提供 whisper.cpp 兼容的 /inference 端点：
# 插件侧 asr_api 保持 "whispercpp"，只改 asr_base_url 即可切换，不动插件代码。
# 实测（2026-11）：300s 音频 24x 实时速度，峰值内存 ~450MB。
FROM python:3.11-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg curl bzip2 && \
    rm -rf /var/lib/apt/lists/*

RUN pip install --no-cache-dir sherpa-onnx soundfile fastapi "uvicorn[standard]" python-multipart

# 模型走 gh-proxy 加速（宿舍网 GitHub 直连约 60KB/s，代理后 ~750KB/s）
RUN mkdir -p /models && \
    curl -sL --retry 3 -o /tmp/sv.tar.bz2 \
      "https://gh-proxy.com/https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17.tar.bz2" && \
    tar -C /models --strip-components=1 -xf /tmp/sv.tar.bz2 \
      sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/model.int8.onnx \
      sherpa-onnx-sense-voice-zh-en-ja-ko-yue-int8-2024-07-17/tokens.txt && \
    curl -sL --retry 3 -o /models/silero_vad.onnx \
      "https://gh-proxy.com/https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx" && \
    rm -f /tmp/sv.tar.bz2

ENV OMP_NUM_THREADS=2 \
    MODEL_DIR=/models \
    PYTHONUNBUFFERED=1

WORKDIR /app
COPY server.py /app/

EXPOSE 50000
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:50000/', timeout=3)" || exit 1

CMD ["uvicorn", "server:app", "--host", "0.0.0.0", "--port", "50000"]
