//! 通用辅助工具集 (General Utilities)
//! 
//! 提供 URI 规范化、物理路径清洗及原子化 I/O 操作。

use base64::prelude::*;
use std::path::Path;
use tokio::fs;
use url::Url;

pub mod subscription;

/// 执行 URI 规范化 (URI Normalization)
/// 
/// 将相对路径或 Protocol-relative URL 转换为绝对定位符。
pub fn to_absolute_url(base: &Url, href: &str) -> String {
    if href.is_empty() {
        return String::new();
    }

    if let Some(path_without_slashes) = href.strip_prefix("//") {
        return format!("{}://{}", base.scheme(), path_without_slashes);
    }

    if href.starts_with("http://") || href.starts_with("https://") {
        return href.to_string();
    }

    base.join(href)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| href.to_string())
}

/// 执行路径清洗与资源哈希化 (Path Sanitization)
/// 
/// 基于 URL 生成长度受限的文件名，并保留原始扩展名。
pub fn generate_filename(url: &str) -> String {
    let mut hash = BASE64_URL_SAFE_NO_PAD.encode(url.as_bytes());
    if hash.len() > 64 {
        hash.truncate(64);
    }

    let ext = Path::new(url)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jpg"); 

    let ext = if let Some(idx) = ext.find('?') {
        &ext[..idx]
    } else {
        ext
    };

    format!("{}.{}", hash, ext)
}

/// 检查物理文件是否存在 (Non-blocking)
pub async fn file_exists(path: impl AsRef<Path>) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// 执行目录递归创建与原子化写入 (Atomic-like Write)
pub async fn save_file(path: impl AsRef<Path>, data: &[u8]) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, data).await?;
    Ok(())
}
