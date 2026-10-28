#!/bin/sh
# 容器内运行: 查证第二轮补考当前是否还存在于两个数据面
Xvfb :98 -screen 0 1600x900x24 -nolisten tcp &
XPID=$!
sleep 1.5
export DISPLAY=:98
python -u /scripts/probe_wenda_tasks.py "https://wenda.zhihuishu.com/stu/courseInfo/studyResource?courseId=11543068&polymas=1" /app/data/wenda_now.json
python -u /scripts/goto_click_dump.py "https://hike-teaching-center.polymas.com/stu-hike/studyTask" - /app/data/study_now.json
kill $XPID 2>/dev/null
