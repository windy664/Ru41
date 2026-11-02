# SenseVoice ASR 服务（CPU 版）
# 为 ru41 lecture 插件提供 whisper.cpp 兼容的 /inference 端点：
# 插件侧 asr_api 保持 "whispercpp"，只改 asr_base_url 即可切换，不动插件代码。
FROM python:3.11-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        ffmpeg libsndfile1 && \
    rm -rf /var/lib/apt/lists/*

# CPU 版 torch（比默认 CUDA 版小一个数量级）
RUN pip install --no-cache-dir torch torchaudio --index-url https://download.pytorch.org/whl/cpu && \
    pip install --no-cache-dir funasr fastapi "uvicorn[standard]" python-multipart soundfile

ENV SENSEVOICE_DEVICE=cpu \
    MODELSCOPE_CACHE=/models \
    OMP_NUM_THREADS=4 \
    PYTHONUNBUFFERED=1

WORKDIR /app
COPY server.py /app/

# 构建期预下载模型（SenseVoiceSmall + fsmn-vad，约 1GB），
# 避免首次启动时在宿舍网络里慢慢拉。
RUN python -c "from funasr import AutoModel; AutoModel(model='iic/SenseVoiceSmall', vad_model='fsmn-vad', device='cpu')"

EXPOSE 50000
HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request; urllib.request.urlopen('http://127.0.0.1:50000/', timeout=3)" || exit 1

CMD ["uvicorn", "server:app", "--host", "0.0.0.0", "--port", "50000"]
