//! 自动刷课的调度器与报告。
//!
//! 一轮 poll 的形状：
//!   1. 问 sidecar「今天该不该刷」（判定在那边做，时区统一）；
//!   2. 该刷 → 同步触发一轮，等它跑完；不该刷 → 只出报告，**不发通知**（日常状态是噪音）；
//!   3. 真刷了或出错了 → 渲染成 markdown，走 ru41 的通知出口。
//!
//! 「刷」这件事本身不在插件里做：平台的「资源已完成」标记是浏览器里的前端 JS 上报的，
//! 请求体还是加密的，没有真实浏览器点不亮 —— 浏览器那一段在 sidecar 里。

use std::sync::Arc;

use ru41::ports::channel::NotifyChannel;

use crate::client::{BrushResult, CourseBrushClient, Status};

/// 刷课调度器（`schedule run course-brush` 的单轮逻辑）。
#[derive(Clone)]
pub struct CourseBrushTracker {
    client: CourseBrushClient,
    notifier: Option<Arc<dyn NotifyChannel>>,
    target: String,
    max_minutes: u64,
    max_courses: u32,
    should_notify: bool,
    interval: std::time::Duration,
}

impl CourseBrushTracker {
    pub fn new(
        client: CourseBrushClient,
        notifier: Option<Arc<dyn NotifyChannel>>,
        target: String,
        max_minutes: u64,
        max_courses: u32,
        should_notify: bool,
        interval: std::time::Duration,
    ) -> Self {
        Self {
            client,
            notifier,
            target,
            max_minutes,
            max_courses,
            should_notify,
            interval,
        }
    }

    async fn notify(&self, text: &str) {
        let Some(notifier) = self.notifier.clone() else {
            return;
        };
        if let Err(error) = notifier.send(&self.target, None, text).await {
            eprintln!("[course-brush] 通知失败: {error:#}");
        }
    }
}

impl ru41::ingress::schedule::SimplePollJob for CourseBrushTracker {
    type Report = serde_json::Value;

    fn interval(&self) -> std::time::Duration {
        // 真正的开工时间由 sidecar 决定（每天 19:00 CST 之后且今天没刷过），
        // 这里只决定「多久问一次」。
        self.interval
    }

    async fn poll(&self) -> anyhow::Result<Self::Report> {
        let status = self
            .client
            .status()
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;

        if !status.due {
            // 今天不用刷：出报告、不通知。
            return Ok(serde_json::json!({
                "action": "skip",
                "reason": status.due_reason,
                "now": status.now_local,
                "markdown": render_skip(&status),
            }));
        }

        let result = self
            .client
            .brush(self.max_minutes, self.max_courses)
            .await
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let after = self.client.status().await.ok();
        let markdown = render_brush(&status, &result, after.as_ref());

        // 真刷了（成或败）都通知 —— 用户要看到结果，尤其是失败。
        if self.should_notify {
            self.notify(&markdown).await;
        }

        Ok(serde_json::json!({
            "action": "brushed",
            "ok": result.ok,
            "exit_code": result.exit_code,
            "elapsed_secs": result.elapsed_secs,
            "error": result.error,
            "markdown": markdown,
        }))
    }
}

// ── 渲染 ─────────────────────────────────────────────────────────────

fn course_lines(status: &Status) -> Vec<String> {
    status
        .courses
        .iter()
        .map(|course| {
            format!(
                "- {}（{}引擎，进度 {}）",
                course.title.as_deref().unwrap_or("未命名"),
                course.engine.as_deref().unwrap_or("?"),
                course
                    .progress
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "未知".to_owned()),
            )
        })
        .collect()
}

fn render_skip(status: &Status) -> String {
    let mut lines = vec![format!(
        "# 智慧树刷课 · 今日无需刷课（{}）",
        status.now_local.as_deref().unwrap_or("")
    )];
    lines.push(format!(
        "- 原因：{}",
        status.due_reason.as_deref().unwrap_or("未说明")
    ));
    if let Some(last) = status.last_round.generated_at.as_deref() {
        lines.push(format!("- 上一轮：{last}"));
    }
    if let Some(ready) = status.report.content_ready.first() {
        lines.push(format!(
            "- 平台侧内容就绪：{}（共 {} 门）",
            ready,
            status.report.content_ready.len()
        ));
    }
    lines.join("\n")
}

fn render_brush(before: &Status, result: &BrushResult, after: Option<&Status>) -> String {
    let status = after.unwrap_or(before);
    let mut lines = vec![format!(
        "# 智慧树刷课 · {}",
        after
            .and_then(|value| value.now_local.clone())
            .unwrap_or_else(|| before.now_local.clone().unwrap_or_default())
    )];
    lines.push(if result.ok {
        format!(
            "- 结果：✅ 跑完一轮（耗时 {} 分钟）",
            result
                .elapsed_secs
                .map(|secs| secs / 60)
                .unwrap_or_default()
        )
    } else {
        format!(
            "- 结果：❌ 这一轮没跑成{}",
            result
                .error
                .as_deref()
                .map(|error| format!("（{error}）"))
                .unwrap_or_default()
        )
    });

    let results = &status.last_round.results;
    if results.is_empty() {
        lines.push("- 本轮没有课程记录".to_owned());
    } else {
        lines.push("- 本轮：".to_owned());
        for item in results {
            let title = item.title.as_deref().unwrap_or("未命名");
            let watched = item.watched.unwrap_or(0);
            let failed = item.failed.unwrap_or(0);
            let mut line = format!("  · {title}：看了/浏览了 {watched} 项");
            if failed > 0 {
                line.push_str(&format!("，失败 {failed} 项"));
            }
            if let Some(error) = item.error.as_deref() {
                line.push_str(&format!("，错误：{error}"));
            }
            lines.push(line);
        }
    }

    let courses = course_lines(status);
    if !courses.is_empty() {
        lines.push("- 目标清单：".to_owned());
        lines.extend(courses);
    }
    if !status.report.content_ready.is_empty() {
        lines.push(format!(
            "- 平台侧内容就绪：{}",
            status.report.content_ready.join("、")
        ));
    }
    lines.join("\n")
}
