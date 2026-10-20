# 学习通协议调研（2026-10）

目标：为 Ru41 选择学习通签到监控的实现路径（sidecar vs 协议端直连）。
结论先行：**纯 Rust 直连**。学习通协议是活的纯 HTTP，4 个端点搞定签到场景，
无需浏览器，形态与 ecard 平台逆向一致；智慧树式 sidecar 的前提（协议死、需浏览器）在这里不成立。

## 生态摸底

| 项目 | 状态 | 结论 |
| --- | --- | --- |
| Samueli924/chaoxing (3.6k★) | 活跃，GPL-3.0 | Python 协议库，先当探针用（已验证登录可用），实现参考 |
| cxOrz/chaoxing-sign-cli | 2025-03 已归档 | 弃 |
| conql/Coppelia39 等 forks | GitHub API `pushed_at` 全部 2023 | 搜索快照标"活跃"不可信，必须查 pushed_at |
| ChaoxingSignFaker (807★) | 活跃 | Android-only，适合手机端不适合 sidecar |

教训（承接 mengxun-sign）：CDN 会拦数据中心 IP，但本次直连实测未被拦（校园网/家宽出口）。

## 协议端点（实测样本存档于 sidecar ~/Project/chaoxing-sidecar）

1. **登录** `POST passport2.chaoxing.com/fanyalogin`
   - form：`uname`/`password` 均 AES-128-CBC（key=iv=`u2oh6Vu^HWe4_AES`，公开值）+ PKCS7 + base64；
     其余 `fid=-1, t=true, forbidotherlogin=0, ...`
   - 响应 `{status: bool, msg2}`；成功后 Set-Cookie：`_uid/_d/vc3/fid/...`（domain 级，跨子域共享）
   - 会话校验：`GET passport2.chaoxing.com/mooc/accountManage` 页面含
     `id="messageName" value="姓名"`，可同时拿到真实姓名
2. **课程列表** `POST mooc2-ans.chaoxing.com/mooc2-ans/visit/courselistdata`
   - form `courseType=1, courseFolderId=0, query="", superstarClass=0`；Referer 需带
     `.../visit/interaction?moocDomain=...`（接口抽风时的历史补丁）
   - 返回 HTML；课程块 `div.course ...`（class 列表以 course 开头），内含
     `input[name=clazzId]`、`input[name=courseId]`、href 的 `cpi=`、`span.course-name[title]`、
     `p.*color3[title]`（教师）；未开放课程带 `not-open-tip`
   - 注意：fid 取 cookie `fid`（缺省 1024），uid 取 `_uid`/`UID`
3. **活动列表** `GET mobilelearn.chaoxing.com/v2/apis/active/student/activelist`
   - query：`fid, courseId, classId, showNotStartedActive=0, _={ms}`
   - 响应 `{result:1, data:{activeList:[...]}}`；关键字段（实测样本）：
     `id`(数值), `activeType`(2=签到), `type`(0普通/1拍照/2二维码/3手势/4位置/5签到码),
     `nameOne`(类型标签), `startTime/endTime`(ms), `status`(1进行中/2结束),
     `userStatus`(1未签), `attendNum`
4. **签到** `GET mobilelearn.chaoxing.com/newsign/preSign`（general=1&sys=1&ls=1&appType=15&uid&activePrimaryId&courseId&classId）
   → `GET /pptSign/stuSignajax`（activeId, uid, fid, courseId, classId, objectId=aaa, latitude/longitude/name 位置类才带）
   - 响应文本 `success` / 已签到提示 / 距离不足提示

## 关键决策记录

- **放弃 sidecar**：智慧树走 sidecar 是因为其协议层死亡（加密+浏览器依赖）；学习通
  协议简单且活着，Rust 内直连少一个进程、少一个文件桥、少一个 Python venv，
  凭证管理回归 ecard 式 `[sources.*]`。
- **Python 探针不浪费**：登录可行性、响应 schema、fid/uid 来源均由探针实测确认；
  Rust 侧 AES 实现用探针产出的密文向量做跨语言一致性测试。
- **风控红线**：无自动代签；二维码/拍照交手机端；cookie 持久化复用、账密登录降频；
  轮询默认 60s（签到窗口 1-2 分钟，更疏会漏），介意可调大 interval_secs。
