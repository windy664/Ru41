"""Paraformer-large ASR HTTP 服务（sherpa-onnx int8 版，无 torch，低内存）。

SenseVoice 版的姊妹服务：接口完全一致（whisper.cpp 兼容），供 A/B 对比。
  GET  /          健康检查
  POST /inference multipart 表单字段 file（音频），返回 {"text": "..."}

与 SenseVoice 的差异：Paraformer 原生输出无标点，
用 CT-Transformer 标点模型（int8）对拼接后的全文补标点。
长音频先经 silero-vad 切段再逐段识别。
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

recognizer = sherpa_onnx.OfflineRecognizer.from_paraformer(
    paraformer=f"{MODEL_DIR}/model.int8.onnx",
    tokens=f"{MODEL_DIR}/tokens.txt",
    num_threads=NUM_THREADS,
    provider="cpu",
)

punct = sherpa_onnx.OfflinePunctuation(
    sherpa_onnx.OfflinePunctuationConfig(
        model=sherpa_onnx.OfflinePunctuationModelConfig(
            ct_transformer=f"{MODEL_DIR}/punct.int8.onnx",
            num_threads=NUM_THREADS,
            provider="cpu",
        )
    )
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
    joined = "".join(texts)
    # Paraformer 原生无标点：对整段拼接结果补标点（CT-Transformer 吃长文本没问题）
    return punct.add_punctuation(joined) if joined else joined


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
