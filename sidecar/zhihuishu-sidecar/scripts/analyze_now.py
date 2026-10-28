"""分析 run_check.sh 的产出: 补考是否还在这两个数据面 (调试用)."""
import json
import re

w = json.load(open("data/wenda_now.json"))
for t in w["traffic"]:
    if "NowExamList" in t["url"] or "homeworkUser" in t["url"]:
        which = t["url"].split("?")[0].split("/")[-1]
        print("WENDA", which, json.loads(t["body"]).get("rt"))
print("补考 in wenda text:", "补考" in w["text"])

s = json.load(open("data/study_now.json"))
print("补考 in studyTask text:", "第二轮" in s.get("text", ""))
m = re.search(r"待完成\((\d+)\)", s.get("text", ""))
print("待完成 count:", m.group(1) if m else "?")
print("--- studyTask text ---")
print(s.get("text", "")[:400])
