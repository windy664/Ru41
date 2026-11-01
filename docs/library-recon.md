# 广软图书馆（libsp）接入侦察笔记

> 目标平台：移动图书馆 https://mfindgzus.libsp.cn/（超星 libsp 系，统一检索）
> 结论：**全链路已打通并实测**（CAS → 超星统一认证 → libsp 会话 → 借阅/历史接口）。
> 本文不含任何凭证；配置见 `ru41.toml`（不入库）。

## 一、登录链路（SSO 四跳）

libsp 自身不管理密码，登录走学校统一身份认证（联奕 CAS，与教务同套）：

```
1. libsp 入口（前端发现无 jwt 时跳这里）
   GET https://unified-auth.chaoxing.com/login_auth/v2/gzus/init
       ?refer=https://findgzus.libsp.cn/find/sso/login/gzus/1
   → 返回 JS 跳转页，302 链到 CAS 登录页

2. CAS 账密登录（与教务 jwxt 模块完全同一套）
   a. GET  https://cas.gzus.edu.cn/lyuapServer/kaptcha?uid=<uuid>
      → {"uid": "...", "content": "data:image/png;base64,..."}
      算术验证码（个位 ±× 个位），本地模板匹配可解，复用
      src/modules/jwxt/captcha.rs（LOO 40/40，线上实测有效）
   b. POST https://cas.gzus.edu.cn/lyuapServer/v1/tickets
      form: username, password=<Ohdave RSA 密文>, service=<超星 service>,
            loginType=<空>, id=<kaptcha uid>, code=<算术答案>
      → {"tgt": "TGT-..."}；密码错返回 data.code=PASSERROR（data:"5,1" =
        已错1次/满5次锁号），验证码错返回 CODEFALSE（不烧密码计数）
      RSA: ohdave_encrypt（source.rs），公钥常量已核对与线上前端 bundle 一致
   c. POST https://cas.gzus.edu.cn/lyuapServer/v1/tickets/<TGT>
      form: service=<超星 service>&loginToken=loginToken
      → 响应体即 ST（ST-...，一次性）

3. 超星统一认证验票（service 必须与换 ST 时一致）
   GET https://unified-auth.chaoxing.com/login_auth/v2/cas/gzus/index?ticket=<ST>
   → 200 HTML「跳转中...」，其中内嵌 AJAX 参数：
     var data = "<32hex>"; var time = "<毫秒>"; var enc = "<32hex大写>";
   a. GET /login_auth/v2/cas/gzus/login?data=&time=&enc=&displayName=&userRole=...
      → {"status":true,"cxuser":{...}}，超星侧会话建立
   b. refer 跳转规则（页面 JS jumpToRefer）：libsp 系 refer 追加
      ?data=<data>&time=<time>&enc=<enc>

4. libsp 落地
   GET https://findgzus.libsp.cn/find/sso/login/gzus/1?data=&time=&enc=
   → 302 到 https://mfindgzus.libsp.cn/#/Home?jwt=<JWT>&jwtHeader=jwtOpacAuth
   ⚠️ 鉴权头名字不是固定的！从落地 URL 的 jwtHeader 参数读取（当前值
   jwtOpacAuth），校方/超星可换；JWT payload 含 sub/groupCode/mappingPath，
   有效期 24h。
```

## 二、API 调用约定

所有业务接口同源 `https://mfindgzus.libsp.cn`，POST JSON，必须带三个头：

| 头 | 值 | 说明 |
|---|---|---|
| `<jwtHeader>`（当前 `jwtOpacAuth`） | `<JWT>` | 鉴权 |
| `groupCode` | `201550` | 学校编码（JWT payload 里也有） |
| `mappingPath` | 空（SPA 根路径时） | 前端按 pathname 生成，保持空串 |

缺鉴权头 → `{"success":false,"errCode":9999,"message":"系统访问中断..."}`；
参数不对 → `success:false, message:"参数异常 {0}"`。

### 实测可用的接口

```
POST /find/loanInfo/loanList          当前借阅
POST /find/loanInfo/loanHistoryList   借阅历史
POST /find/lendbook/reNew             续借（P0 提醒按钮的目标接口）
```

loanList 请求体（前端实际抓包）：

```json
{"page":1,"rows":20,"searchType":1,"searchContent":"","sortType":0,
 "startDate":null,"endDate":null}
```

⚠️ searchType/searchContent/sortType 三个字段缺一不可，只传分页会报「参数异常」。
响应：`{"success":true,"data":{"searchResult":[],"loanNum":10,"numFound":0}}`
（loanNum=还可借册数，numFound=当前在借数）

历史记录字段（实测样例，在借记录同构）：

```
recordId/loanId, title, author, publisher, isbn, publishYear,
loanDate(借出), normReturnDate(应还日期★催还提醒用), returnDate(已还),
locationName(馆藏地如"计算机书库"), barcode/propNo, docCode, libCode
```

## 三、与 CAS 相关的两个坑（已踩实）

1. **PASSERROR 计数**：CAS 密码错满 5 次锁号，自动登录每 poll 烧一次；
   `cas_login` 对 PASSERROR/USERLOCK 必须立即停止重试（现状已如此）。
2. **验证码 uid**：kaptcha 响应里的 uid 可能与请求的不同，以响应为准提交。

## 四、容器侧 DNS 钉子教训（2026-11-01 事故）

jwxt 连续 5 天 `login_ok:false`，根因：ru41-jwxt 容器 `HostConfig.ExtraHosts`
里有历史遗留的 `jwxt.gzus.edu.cn:110.64.203.222` —— .222 是教务**旧版节点**
（根路径 200、新接口 405），新版节点为 .223。CAS 登录/验票均成功，但课表
接口永远被踢回登录页。修复：容器重建时 ExtraHosts 改为 .223（cas 保持 .222，
ecarduser 改 .111 与 ecard 容器一致）。`ru41-roll.py` 按 ExtraHosts 原样重建，
所以改 ExtraHosts 即持久化。

排查链路时的对照手段（全部有效）：`getent ahostsv4 <host>` 对比宿主/容器解析、
`curl --resolve` 钉 IP 手动走 SSO、`docker exec -w /home/node ru41-jwxt ru41
--config /etc/ru41/ru41.toml --db /data/state.db jwxt poll` 以守护进程同参
手动触发（⚠️ 不带 --db 会落到容器内不存在的本地路径，读到空库假象）。

## 五、附属规划（已对齐）

- P0：到期催还提醒（poll loanList，应还前 3/1/0 天提醒）+ 提醒消息带「续借」
  按钮（复用 ports::commands::ReplyMarkup，点击=以用户身份发 `/续借 书名`）
- P0.5：`/借阅` 命令；P1：`/找书`（searchList + 馆藏地）；P1.5：站内通知转发
- 结构：纯 HTTP 轮询即可，无需 sidecar；复用 jwxt 的 captcha/ohdave 组件
