//! 校园通知要素抽取：班会/宣讲会通知有明显文字锚点
//!（「通知」标题行 + ⏰时间/时间： + 🏠地点/地点：），规则抽取比 LLM 稳。
//! 一条消息可能含多则通知（转发拼接），逐锚点产出多条。

/// 一则通知的抽取结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoticeInfo {
    pub title: String,
    /// 归一化后的时间文本，直接喂给 domain::schedule::parse_event_when。
    pub when_text: String,
    pub location: Option<String>,
}

/// 抽取消息里的全部通知；没有可识别通知时返回空 Vec。
pub fn extract_notices(content: &str) -> Vec<NoticeInfo> {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let Some(when_raw) = after_anchor(line, &["⏰时间", "⏰", "时间：", "时间:"])
        else {
            continue;
        };
        let when_text = normalize_when(when_raw);
        if !looks_like_schedule(&when_text) {
            continue;
        }
        let title = find_title(&lines, idx);
        let Some(title) = title else { continue };
        let location = find_location(&lines, idx);
        out.push(NoticeInfo {
            title,
            when_text,
            location,
        });
    }
    out
}

/// 取锚点之后的文本（锚点按其在中国校园文化里的实际写法枚举）。
fn after_anchor<'a>(line: &'a str, anchors: &[&str]) -> Option<&'a str> {
    for anchor in anchors {
        if let Some(pos) = line.find(anchor) {
            let rest = &line[pos + anchor.len()..];
            return Some(rest.trim_start_matches(['：', ':', ' ']));
        }
    }
    None
}

/// 时间文本归一化：去括号注记（本周五）、去星期词、日期点号转连字符、
/// 全角波浪/冒号转半角——对齐 parse_event_when 支持的格式。
/// 删除的词一律替换成空格，避免前后文本粘连（「10.29周四19:30」→「10-29 19:30」）。
fn normalize_when(raw: &str) -> String {
    let mut s = raw.to_owned();
    // 括号注记：（本周五 ）、(暂定) 等
    while let Some(start) = s.find(['（', '(']) {
        let close = if s[start..].starts_with('（') {
            '）'
        } else {
            ')'
        };
        match s[start..].find(close) {
            Some(end) => {
                s.replace_range(start..start + end + close.len_utf8(), " ");
            }
            None => break,
        }
    }
    // 星期词：周四 / 本周四 / 周三下午
    for day in ["一", "二", "三", "四", "五", "六", "日", "天"] {
        s = s
            .replace(&format!("本周{day}"), " ")
            .replace(&format!("周{day}"), " ");
    }
    // 日期点号 10.29 → 10-29（时刻用冒号不受影响）
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c == '.'
            && i > 0
            && i + 1 < chars.len()
            && chars[i - 1].is_ascii_digit()
            && chars[i + 1].is_ascii_digit()
        {
            out.push('-');
        } else {
            out.push(*c);
        }
    }
    // 收敛空白：多余空格合并，首尾裁剪
    let out = out.replace('～', "~").replace('：', ":");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 像不像一个可解析的排期：至少有一个时刻（HH:MM 或 N点）。
fn looks_like_schedule(text: &str) -> bool {
    let has_clock = text
        .split(|c: char| !(c.is_ascii_digit() || c == ':'))
        .any(|tok| tok.contains(':') && tok.len() >= 4)
        || text.contains('点');
    has_clock && text.chars().any(|c| c.is_ascii_digit())
}

/// 标题：时间锚点之前最近的「通知」行；否则最近含《》的行（讲座/活动名常在
/// 书名号里）；再否则最近的非空、非「1、」式条目行（条目行是字段不是标题）。
/// 清理：去前导 emoji/符号、去「关于」前缀、去「的通知/补充通知/通知」后缀。
fn find_title(lines: &[&str], anchor_idx: usize) -> Option<String> {
    let candidates = &lines[..anchor_idx];
    let is_detail_line = |l: &&str| {
        let t = l.trim_start();
        match t.find('、') {
            // 「1、」「12、」式枚举前缀（序号最多两位）
            Some(pos) => pos <= 2 && t[..pos].chars().all(|c| c.is_ascii_digit()),
            None => false,
        }
    };
    let pick = candidates
        .iter()
        .rposition(|l| l.contains("通知"))
        .or_else(|| candidates.iter().rposition(|l| l.contains('《')))
        .or_else(|| {
            candidates
                .iter()
                .rposition(|l| !l.trim().is_empty() && !is_detail_line(l))
        })
        .or_else(|| candidates.iter().rposition(|l| !l.trim().is_empty()))?;
    let raw = candidates[pick];
    // 书名号内容优先作标题（剥掉「（二）xx学院承办的」这类前缀）
    if let Some(inner) = raw.split('《').nth(1).and_then(|s| s.split('》').next()) {
        let inner = inner.trim();
        if !inner.is_empty() {
            return Some(inner.chars().take(40).collect());
        }
    }
    // 前导 emoji/符号剥掉；char::is_alphanumeric 对 CJK 已为真，无需单独判断。
    let stripped: String = raw.chars().skip_while(|c| !c.is_alphanumeric()).collect();
    let stripped = stripped
        .trim_start_matches("关于")
        .trim_end_matches("的补充通知")
        .trim_end_matches("通知")
        .trim();
    if stripped.is_empty() {
        return None;
    }
    Some(stripped.chars().take(40).collect())
}

/// 地点：时间锚点之后、下一则通知之前的 🏠/地点 行。
fn find_location(lines: &[&str], anchor_idx: usize) -> Option<String> {
    for line in &lines[anchor_idx + 1..] {
        if after_anchor(line, &["⏰", "时间：", "时间:"]).is_some() {
            break; // 下一则通知开始了
        }
        if let Some(rest) = after_anchor(line, &["🏠地点", "🏠", "地点：", "地点:"]) {
            let loc: String = rest
                .trim()
                .chars()
                .take_while(|c| *c != '\n')
                .collect::<String>()
                .trim()
                .to_owned();
            if !loc.is_empty() {
                return Some(loc);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 班会通知样本() {
        let msg = "📣班会通知\n🛑🌟为响应学校号召建设星级班级，我们将开启星级班级——文明之星动员班会。\n⏰时间：10.29周四19:30～20:30\n🏠地点：A203\n🔆注意：若无法到场请提前向班长说明情况";
        let notices = extract_notices(msg);
        assert_eq!(notices.len(), 1);
        let n = &notices[0];
        assert_eq!(n.title, "班会");
        assert_eq!(n.when_text, "10-29 19:30~20:30");
        assert_eq!(n.location.as_deref(), Some("A203"));
    }

    #[test]
    fn 宣讲会通知样本() {
        let msg = "✨关于第一届校园AI智能体创新应用大赛线下宣讲会的补充通知\n⏰时间：10月30日（本周五 ）13:00—14:00\n🏠地点：T105（广州校区）\n⭕️请有意向的同学填写下列在线表格进行报名";
        let notices = extract_notices(msg);
        assert_eq!(notices.len(), 1);
        let n = &notices[0];
        assert_eq!(n.title, "第一届校园AI智能体创新应用大赛线下宣讲会");
        assert_eq!(n.when_text, "10月30日 13:00—14:00");
        assert_eq!(n.location.as_deref(), Some("T105（广州校区）"));
    }

    #[test]
    fn 一条消息两则通知() {
        let msg = "📣班会通知\n🛑🌟我们将开启星级班级——文明之星动员班会。\n⏰时间：10.29周四19:30～20:30\n🏠地点：A203\n✨关于第一届校园AI智能体创新应用大赛线下宣讲会的补充通知\n⏰时间：10月30日（本周五 ）13:00—14:00\n🏠地点：T105（广州校区）";
        let notices = extract_notices(msg);
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].title, "班会");
        assert_eq!(notices[1].title, "第一届校园AI智能体创新应用大赛线下宣讲会");
        assert_eq!(notices[1].location.as_deref(), Some("T105（广州校区）"));
    }

    #[test]
    fn 闲聊不误判() {
        assert!(extract_notices("我下午有没有课").is_empty());
        assert!(extract_notices("今天天气怎么样").is_empty());
        assert!(extract_notices("通知你一下，记得带钥匙").is_empty());
    }

    /// 回归（2026-10-30 真实漏识别）：讲座通知没有「通知」字样、正文有换行续行，
    /// 时间带「年」。标题应取《》书名号内容，时间应解析到对应日期。
    #[test]
    fn 讲座通知样本() {
        let msg = "（二）电子信息与控制工程学院承办的《智能时代下，普通信息类本科生的AI技术落地与就业机会》\n\n1、讲座主题：智能时代下，普通信息类本科生的AI技术落地与就\n\n            业机会\n\n2、讲座时间：2026年11月3日（星期二）15:30\n\n3、讲座地点：礼堂一（广州校区）\n\n4、主讲人：赖剑煌";
        let notices = extract_notices(msg);
        assert_eq!(notices.len(), 1);
        let n = &notices[0];
        assert_eq!(n.title, "智能时代下，普通信息类本科生的AI技术落地与就业机会");
        assert_eq!(n.when_text, "2026年11月3日 15:30");
        assert_eq!(n.location.as_deref(), Some("礼堂一（广州校区）"));
        let parsed = ru41::domain::schedule::parse_event_when(
            &n.when_text,
            1793350000,
            &ru41::domain::todo::TodoOptions::default(),
        );
        assert!(parsed.is_some(), "带「年」的日期必须可解析");
    }
}
