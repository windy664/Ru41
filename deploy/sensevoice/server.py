"""SenseVoice ASR HTTP 服务（sherpa-onnx int8 版，无 torch，低内存）。

对 ru41 lecture 插件暴露 whisper.cpp 兼容接口：
  GET  /          健康检查
  POST /inference multipart 表单字段 file（音频），返回 {"text": "..."}

长音频先经 silero-vad 切段再逐段识别（SenseVoice 单次输入不宜超过 30s）。
int8 量化模型常驻内存约几百 MB，适合低功耗 CPU 小机器。
"""

import os
import re
import subprocess
import tempfile

import numpy as np
import sherpa_onnx
import soundfile as sf
from fastapi import FastAPI, File, UploadFile

MODEL_DIR = os.environ.get("MODEL_DIR", "/models")
NUM_THREADS = int(os.environ.get("OMP_NUM_THREADS", "2"))
SAMPLE_RATE = 16000
# SenseVoice 单段最长 30s，留余量切 20s
MAX_SPEECH_S = float(os.environ.get("MAX_SPEECH_S", "20"))

recognizer = sherpa_onnx.OfflineRecognizer.from_sense_voice(
    model=f"{MODEL_DIR}/model.int8.onnx",
    tokens=f"{MODEL_DIR}/tokens.txt",
    num_threads=NUM_THREADS,
    use_itn=True,
    provider="cpu",
)

vad_config = sherpa_onnx.VadModelConfig()
vad_config.silero_vad.model = f"{MODEL_DIR}/silero_vad.onnx"
vad_config.silero_vad.threshold = 0.5
vad_config.silero_vad.min_speech_duration = 0.25
vad_config.silero_vad.min_silence_duration = 0.5
vad_config.silero_vad.max_speech_duration = MAX_SPEECH_S
vad_config.sample_rate = SAMPLE_RATE

app = FastAPI()
TAG_RE = re.compile(r"<\|.*?\|>")


@app.get("/")
def health():
    return {"status": "ok"}


def load_audio_16k(path: str) -> np.ndarray:
    """任意格式转 16k 单声道 float32。"""
    with tempfile.NamedTemporaryFile(delete=False, suffix=".wav") as tmp:
        wav_path = tmp.name
    try:
        subprocess.run(
            ["ffmpeg", "-y", "-i", path, "-ar", str(SAMPLE_RATE), "-ac", "1", "-f", "wav", wav_path],
            check=True, capture_output=True,
        )
        samples, _ = sf.read(wav_path, dtype="float32")
        return samples
    finally:
        os.unlink(wav_path)


def transcribe_samples(samples: np.ndarray) -> str:
    # VAD 是有状态的，按请求新建，缓冲区给足整段音频长度
    vad = sherpa_onnx.VoiceActivityDetector(
        vad_config, buffer_size_in_seconds=len(samples) / SAMPLE_RATE + 30
    )
    window = vad_config.silero_vad.window_size
    texts = []
    pos = 0
    while pos + window <= len(samples):
        vad.accept_waveform(samples[pos : pos + window])
        pos += window
    vad.flush()
    while not vad.empty():
        stream = recognizer.create_stream()
        stream.accept_waveform(SAMPLE_RATE, vad.front.samples)
        vad.pop()
        recognizer.decode_stream(stream)
        text = TAG_RE.sub("", stream.result.text).strip()
        if text:
            texts.append(text)
    return "".join(texts)


@app.post("/inference")
async def inference(file: UploadFile = File(...)):
    suffix = os.path.splitext(file.filename or "audio.mp3")[1] or ".mp3"
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as tmp:
        tmp.write(await file.read())
        path = tmp.name
    try:
        return {"text": transcribe_samples(load_audio_16k(path))}
    finally:
        os.unlink(path)
