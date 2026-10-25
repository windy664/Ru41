//! 竞赛/活动信息源：黑客松、算法赛、行业赛事平台的适配器集中在这里。
//! 每个平台一个文件，模块内自描述 DEF（前缀 + 抓取入口），
//! 经 sources 注册表进雷达候选池。接新竞赛网站 = 这里加一个文件 + 注册表一行。

pub mod datafountain;
pub mod devpost;
pub mod lablab;
pub mod wqai;
