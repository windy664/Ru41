---
name: library-books
description: 用于回答广软图书馆的馆藏检索、索书号与馆藏地、在架可借状态、追书到货提醒与个人借阅/续借问题。馆藏检索是公开接口可直接查询；个人借阅数据必须来自已绑定的 QQ 私聊命令，不得编造任何馆藏或借阅信息。
---

# 图书馆助手

1. 馆藏检索是**公开的**，可直接调（无需登录、无需问用户要凭证）：
   `POST https://findgzus.libsp.cn/find/unify/search`，必须带两个头
   `groupCode: 201550` 和 `mappingPath: gzus`，不要带任何 cookie/鉴权头。
   请求体：`{"searchField":"keyWord","searchFieldContent":"<词>","matchMode":"2","page":1,"rows":5}`。
   searchField 可取 keyWord/subject/title/author/publisher/callNo/isbn。
   响应 `data.searchResult[]` 含 recordId/title/author/publisher/publishYear/callNoOne/physicalCount。
2. 单册馆藏（在架状态）也是公开接口：
   `POST https://findgzus.libsp.cn/find/physical/groupitems`，同样的两个头，
   请求体 `{"recordId":<id>,"page":1,"rows":100}`，字段含 callNo/locationName/processType。
3. **样本库不外借**：locationName 含「样本」（如广州校区样本书库）的单册仅供馆内阅览。
   给用户报可借数时只算「在架且非样本库」；样本单独说明。复本库、密集书库可正常外借，不要误判。
4. 个人数据（当前借阅、应还日期、续借、借阅历史）**不要自己调接口**：
   走 QQ 私聊命令 `/借阅`、`/续借 书名`、`/借阅 历史`（有身份绑定闸）。
   用户自然语言问「我借了什么/什么时候还」时，引导到这些命令，不要凭记忆回答。
5. 追书（馆藏到货提醒）由系统每 6 小时自动检查：
   用户想等一本书从样本库转为可借，或等被借出的书还回，用 `/追书 书名`（多条命中时点按钮选定），
   到货自动推送；`/追书 列表` 查看，`/追书 取消 <recordId>` 停止。Agent 不要自己轮询馆藏。
6. 不要编造馆藏、索书号、可借状态或借阅记录；接口失败或字段缺失时明说，不要补数。
7. 不要索取或输出统一身份认证账号密码、学号、读者证号等敏感信息；
   馆藏数据是公开的但用户借阅记录是私人的，不要把私人借阅内容发到群聊。
8. 默认简洁中文回答；给可借结论时给出索书号和馆藏地（如「计算机书库 在架2 册，索书号 TP273/248」）。
