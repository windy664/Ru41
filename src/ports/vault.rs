use anyhow::Result;

/// 作业任务目录策略：`All` 每个任务都建目录（旧行为）；
/// `Substantive` 只有实质作业建目录——平台纯在线任务（课前测等）不建，
/// 减少 vault 噪音。由配置 `vault_task_dirs = "all" | "substantive"` 选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VaultTaskDirs {
    #[default]
    All,
    Substantive,
}

impl VaultTaskDirs {
    /// 由配置串解析；非法值报错让配置错误在启动期暴露。
    pub fn parse(raw: Option<&str>) -> anyhow::Result<Self> {
        match raw {
            None | Some("all") => Ok(Self::All),
            Some("substantive") => Ok(Self::Substantive),
            Some(other) => anyhow::bail!(
                "unknown vault task dirs policy {other:?} (expected all or substantive)"
            ),
        }
    }
}

/// Vault 写入服务的端口：所有附属（digest 落 Vault、周报、博客管线等）经它读写
/// Vault，保证「单一写入服务」的串行与幂等。实现当前为 git 工作副本（adapters::vault）。
///
/// 全部为同步阻塞调用：长驻轮询由调度层包 spawn_blocking，QQ 命令路径在 block_in_place 上下文中。
pub trait Vault: Send + Sync {
    /// 拉取远端引用；不动工作区。
    fn fetch(&self) -> Result<()>;
    /// 远端分支 HEAD 的 SHA。
    fn remote_head(&self) -> Result<String>;
    /// 指定提交的标题行（`[ru41]` 前缀用于识别 Ru41 自己的提交）。
    fn head_subject(&self, sha: &str) -> Result<String>;
    /// from..to 之间变更的文件列表（相对路径）。
    fn changed_files(&self, from: &str, to: &str) -> Result<Vec<String>>;
    /// from..to 之间变更的文件（相对路径，true = 删除）。
    /// 不做重命名跟踪（--no-renames）：移出目录 = 旧路径删除 + 新路径新增，
    /// 支撑「移出目录即下架/取消」类语义。
    fn changed_entries(&self, from: &str, to: &str) -> Result<Vec<(String, bool)>>;
    /// 工作区硬对齐到指定提交；未提交内容会被丢弃。
    fn checkout_hard(&self, sha: &str) -> Result<()>;
    /// 暂存全部变更并提交；无可提交内容返回 false。
    fn commit_all(&self, message: &str) -> Result<bool>;
    /// 推送到远端分支。
    fn push(&self) -> Result<()>;
    /// 本地与远端对齐：`pull --rebase` 把本地未推提交重放到远端之上，
    /// 再把仍领先的提交推上去；返回是否有提交被推送。
    /// rebase 冲突时中止并返回错误（工作区恢复原状），由调用方决定告警。
    fn sync(&self) -> Result<bool>;
    /// 读取 Vault 内文件；不存在返回 None。只读不入锁。
    fn read_file(&self, relative: &str) -> Result<Option<String>>;
    /// 写入 Vault 内文件（自动创建父目录）；不写锁外目录，提交推送由调用方负责。
    fn write_file(&self, relative: &str, content: &str) -> Result<()>;
    /// 写入 Vault 内二进制文件（附件等）；路径与锁语义同 `write_file`。
    fn write_bytes(&self, relative: &str, data: &[u8]) -> Result<()>;
    /// 删除 Vault 内文件；不存在返回 false。
    fn remove_file(&self, relative: &str) -> Result<bool>;
    /// 向 Inbox 快速记录笔记追加一行；返回相对路径。
    fn append_inbox_note(
        &self,
        inbox_dir: &str,
        text: &str,
        now: i64,
        offset_seconds: i32,
    ) -> Result<String>;
}
