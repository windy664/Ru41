use anyhow::Result;

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
    /// 工作区硬对齐到指定提交；未提交内容会被丢弃。
    fn checkout_hard(&self, sha: &str) -> Result<()>;
    /// 暂存全部变更并提交；无可提交内容返回 false。
    fn commit_all(&self, message: &str) -> Result<bool>;
    /// 推送到远端分支。
    fn push(&self) -> Result<()>;
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
