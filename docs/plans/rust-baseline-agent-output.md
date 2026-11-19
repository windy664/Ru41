这是 agent 按 `plan-decompose` skill 规则产出的拆解结果（真实样本，2026-11-20）。

校验记录：
- 5 个里程碑全部覆盖，名字与文档一致
- 20 个 step，时长全为正整数
- 合计 1920 分钟（32 小时）
- 比最初手写估计的 24 小时多 1/3 —— **粗估会系统性低估**

```ru41-plan-decompose
{"milestones":[
 {"name":"M1 环境与基本语法","steps":[
   {"name":"读 Book Ch1（安装 + cargo + hello world）","minutes":60},
   {"name":"读 Book Ch3.1-3.2（变量与类型）","minutes":90},
   {"name":"读 Book Ch3.3-3.5（函数与控制流）","minutes":60},
   {"name":"动手：shadowing / if-match 循环练习","minutes":90}]},
 {"name":"M2 所有权与借用","steps":[
   {"name":"读 Book Ch4.1（所有权三规则 + move）","minutes":90},
   {"name":"读 Book Ch4.2（引用与借用）","minutes":120},
   {"name":"动手：借用检查器三类错误实操","minutes":120},
   {"name":"动手：切片 + struct 练习","minutes":90}]},
 {"name":"M3 类型系统","steps":[
   {"name":"读 Book Ch5-6（struct / enum / match）","minutes":90},
   {"name":"动手：Option 与 Result 练习","minutes":90},
   {"name":"读 Book Ch9（错误处理 + ? 运算符）","minutes":90},
   {"name":"动手：写一个带自定义错误的模块","minutes":60}]},
 {"name":"M4 泛型与生命周期","steps":[
   {"name":"读 Book Ch10.1-10.2（泛型 / trait）","minutes":90},
   {"name":"读 Book Ch10.3（生命周期）","minutes":90},
   {"name":"动手：trait + 泛型约束练习","minutes":60},
   {"name":"动手：修 5 个生命周期标注错误","minutes":90}]},
 {"name":"M5 动手写一个 CLI 工具","steps":[
   {"name":"读 Book Ch12（minigrep 全流程）","minutes":90},
   {"name":"实现 minigrep 主体","minutes":180},
   {"name":"补测试 + 错误处理","minutes":90},
   {"name":"动手：给 Rust 计划补 12 个示例的完整测试","minutes":180}]}
]}
```
