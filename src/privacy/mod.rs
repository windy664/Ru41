use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Arc};

/// 一条脱敏映射：假值 → 真值。只存本地，绝不发给 Agent。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingEntry {
    pub rule: String,
    pub fake: String,
    pub real: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Mapping {
    pub entries: Vec<MappingEntry>,
}

impl Mapping {
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 各规则命中次数，用于日志与审计（不含原值）。
    pub fn rule_counts(&self) -> Vec<(String, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for entry in &self.entries {
            *counts.entry(entry.rule.as_str()).or_default() += 1;
        }
        let mut counts: Vec<_> = counts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        counts.sort();
        counts
    }
}

/// 出站脱敏与入站回填边界；映射表只留在调用侧。
pub trait Sanitizer: Send + Sync {
    fn sanitize(&self, text: &str) -> (String, Mapping);
    fn restore(&self, text: &str, mapping: &Mapping) -> String;
}

impl<T: Sanitizer + ?Sized> Sanitizer for Arc<T> {
    fn sanitize(&self, text: &str) -> (String, Mapping) {
        (**self).sanitize(text)
    }
    fn restore(&self, text: &str, mapping: &Mapping) -> String {
        (**self).restore(text, mapping)
    }
}

/// 不脱敏的直通实现（调试开关用）。
pub struct NoopSanitizer;
impl Sanitizer for NoopSanitizer {
    fn sanitize(&self, text: &str) -> (String, Mapping) {
        (text.to_owned(), Mapping::default())
    }
    fn restore(&self, text: &str, _: &Mapping) -> String {
        text.to_owned()
    }
}

#[derive(Clone, Copy)]
enum Generator {
    Phone,
    IdCard,
    Email,
    BankCard,
    Name,
}

struct Rule {
    name: &'static str,
    pattern: Regex,
    /// 命中的敏感值所在捕获组（姓名规则带前缀，只替换组内部分）。
    group: usize,
    generator: Generator,
    enabled: bool,
}

/// 默认脱敏器：命中规则的敏感信息替换为语法合法的随机同类假数据。
pub struct RandomSanitizer {
    rules: Vec<Rule>,
}

impl Default for RandomSanitizer {
    fn default() -> Self {
        Self {
            rules: vec![
                Rule {
                    name: "id_card",
                    pattern: Regex::new(r"\b\d{17}[\dXx]\b").unwrap(),
                    group: 0,
                    generator: Generator::IdCard,
                    enabled: true,
                },
                Rule {
                    name: "phone",
                    pattern: Regex::new(r"\b1[3-9]\d{9}\b").unwrap(),
                    group: 0,
                    generator: Generator::Phone,
                    enabled: true,
                },
                Rule {
                    name: "email",
                    pattern: Regex::new(r"\b[\w.+-]+@[\w-]+(?:\.[\w-]+)+\b").unwrap(),
                    group: 0,
                    generator: Generator::Email,
                    enabled: true,
                },
                Rule {
                    name: "bank_card",
                    pattern: Regex::new(r"\b\d{16,19}\b").unwrap(),
                    group: 0,
                    generator: Generator::BankCard,
                    enabled: true,
                },
                Rule {
                    name: "name",
                    pattern: Regex::new(r"(?:姓名|名字|联系人|收货人)[:：是]?\s*([\p{Han}]{2,3})")
                        .unwrap(),
                    group: 1,
                    generator: Generator::Name,
                    enabled: true,
                },
            ],
        }
    }
}

impl RandomSanitizer {
    /// 开关某类规则（如 `rule("name", false)`）。
    pub fn rule(mut self, name: &str, enabled: bool) -> Self {
        for rule in &mut self.rules {
            if rule.name == name {
                rule.enabled = enabled;
            }
        }
        self
    }
}

impl Sanitizer for RandomSanitizer {
    fn sanitize(&self, text: &str) -> (String, Mapping) {
        let mut result = text.to_owned();
        let mut mapping = Mapping::default();
        let mut real_to_fake: HashMap<String, String> = HashMap::new();
        let mut fakes: Vec<String> = Vec::new();
        let mut rng = rand::rng();
        for rule in self.rules.iter().filter(|r| r.enabled) {
            result = rule
                .pattern
                .replace_all(&result, |captures: &regex::Captures| {
                    let Some(group) = captures.get(rule.group) else {
                        return captures[0].to_owned();
                    };
                    let real = group.as_str();
                    // 前序规则生成的假值不再二次脱敏。
                    if fakes.iter().any(|fake| fake == real) {
                        return captures[0].to_owned();
                    }
                    let fake = real_to_fake.entry(real.to_owned()).or_insert_with(|| {
                        // 假值不得与原文任何片段或既有假值碰撞。
                        let mut candidate = generate(rule.generator, &mut rng);
                        let mut tries = 0;
                        while (text.contains(&candidate) || fakes.contains(&candidate))
                            && tries < 100
                        {
                            candidate = generate(rule.generator, &mut rng);
                            tries += 1;
                        }
                        fakes.push(candidate.clone());
                        mapping.entries.push(MappingEntry {
                            rule: rule.name.to_owned(),
                            fake: candidate.clone(),
                            real: real.to_owned(),
                        });
                        candidate
                    });
                    let full = captures.get(0).unwrap();
                    let offset = group.start() - full.start();
                    format!(
                        "{}{}{}",
                        &full.as_str()[..offset],
                        fake,
                        &full.as_str()[offset + group.len()..]
                    )
                })
                .into_owned();
        }
        (result, mapping)
    }

    fn restore(&self, text: &str, mapping: &Mapping) -> String {
        let mut restored = text.to_owned();
        let mut entries: Vec<_> = mapping.entries.iter().collect();
        // 先长后短，避免假值互为前缀时误替换。
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.fake.len()));
        for entry in entries {
            restored = restored.replace(&entry.fake, &entry.real);
        }
        restored
    }
}

fn generate(generator: Generator, rng: &mut impl Rng) -> String {
    match generator {
        Generator::Phone => format!(
            "1{}{:09}",
            rng.random_range(3..=9),
            rng.random_range(0..=999_999_999)
        ),
        Generator::IdCard => generate_id_card(rng),
        Generator::Email => {
            const DOMAINS: &[&str] = &["example.com", "mail.example.org", "demo.example.net"];
            let local: String = (0..8)
                .map(|_| (b'a' + rng.random_range(0..26)) as char)
                .collect();
            format!("{}@{}", local, DOMAINS[rng.random_range(0..DOMAINS.len())])
        }
        Generator::BankCard => generate_bank_card(rng),
        Generator::Name => {
            const SURNAMES: &[&str] = &["王", "李", "张", "刘", "陈", "杨", "黄", "赵", "吴", "周"];
            const GIVEN: &[&str] = &[
                "伟", "芳", "娜", "敏", "静", "磊", "洋", "勇", "杰", "涛", "明", "超",
            ];
            let surname = SURNAMES[rng.random_range(0..SURNAMES.len())];
            let given: String = (0..rng.random_range(1..=2))
                .map(|_| GIVEN[rng.random_range(0..GIVEN.len())])
                .collect();
            format!("{surname}{given}")
        }
    }
}

/// 身份证号：合法区划 + 出生日期 + 顺序码 + GB11643 校验位。
fn generate_id_card(rng: &mut impl Rng) -> String {
    const AREAS: &[&str] = &["110101", "310104", "440305", "320102", "420106"];
    let area = AREAS[rng.random_range(0..AREAS.len())];
    let year = rng.random_range(1970..=2000);
    let month = rng.random_range(1..=12);
    let day = rng.random_range(1..=28);
    let seq = rng.random_range(0..=999);
    let body = format!("{area}{year:04}{month:02}{day:02}{seq:03}");
    const WEIGHTS: &[u32] = &[7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    const CHECKS: &[char] = &['1', '0', 'X', '9', '8', '7', '6', '5', '4', '3', '2'];
    let sum: u32 = body
        .bytes()
        .zip(WEIGHTS)
        .map(|(digit, weight)| u32::from(digit - b'0') * weight)
        .sum();
    format!("{}{}", body, CHECKS[(sum % 11) as usize])
}

/// 银行卡号：62 开头 16 位，Luhn 校验。
fn generate_bank_card(rng: &mut impl Rng) -> String {
    let body: String = std::iter::once("62".to_owned())
        .chain((0..13).map(|_| rng.random_range(0..=9).to_string()))
        .collect();
    let sum: u32 = body
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, digit)| {
            let mut value = u32::from(digit - b'0');
            if i % 2 == 0 {
                value *= 2;
                if value > 9 {
                    value -= 9;
                }
            }
            value
        })
        .sum();
    let check = (10 - sum % 10) % 10;
    format!("{body}{check}")
}
