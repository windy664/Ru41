#!/bin/sh
# 容器内运行: studyTask 页点「全部任务」标签, 看第二轮补考的最终状态
Xvfb :98 -screen 0 1600x900x24 -nolisten tcp &
XPID=$!
sleep 1.5
export DISPLAY=:98
python -u /scripts/goto_click_dump.py "https://hike-teaching-center.polymas.com/stu-hike/studyTask" "全部任务" /app/data/study_all.json
kill $XPID 2>/dev/null
