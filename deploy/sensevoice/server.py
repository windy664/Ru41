"""SenseVoice ASR HTTP 服务。

对 ru41 lecture 插件暴露 whisper.cpp 兼容接口：
  GET  /          健康检查
  POST /inference multipart 表单字段 file（音频），返回 {"text": "..."}

长音频由内置 FSMN-VAD 切段，SenseVoiceSmall 非自回归推理，
CPU 上约为 15~17 倍实时速度，远快于 whisper.cpp。
"""

import os
import re
import tempfile

import torch
from fastapi import FastAPI, File, UploadFile
from funasr import AutoModel
from funasr.utils.postprocess_utils import rich_transcription_postprocess

# 服务器电池已坏、纯靠适配器供电，满载断电有前科：线程数保守一点。
torch.set_num_threads(int(os.environ.get("OMP_NUM_THREADS", "4")))

model = AutoModel(
    model="iic/SenseVoiceSmall",
    vad_model="fsmn-vad",
    vad_kwargs={"max_single_segment_time": 30000},
    device=os.environ.get("SENSEVOICE_DEVICE", "cpu"),
    disable_update=True,
)

app = FastAPI()
TAG_RE = re.compile(r"<\|.*?\|>")


@app.get("/")
def health():
    return {"status": "ok"}


@app.post("/inference")
async def inference(file: UploadFile = File(...)):
    suffix = os.path.splitext(file.filename or "audio.mp3")[1] or ".mp3"
    with tempfile.NamedTemporaryFile(delete=False, suffix=suffix) as tmp:
        tmp.write(await file.read())
        path = tmp.name
    try:
        res = model.generate(
            input=path,
            cache={},
            language="auto",
            use_itn=True,
            batch_size_s=60,
            merge_vad=True,
            merge_length_s=15,
        )
        text = rich_transcription_postprocess(res[0]["text"]) if res else ""
        return {"text": TAG_RE.sub("", text).strip()}
    finally:
        os.unlink(path)
