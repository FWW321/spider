use base64::prelude::*;
use std::path::Path;
use tokio::fs;
use url::Url;

pub mod singbox;
pub mod subscription;

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

pub fn generate_filename(url: &str) -> String {
    let mut hash = BASE64_URL_SAFE_NO_PAD.encode(url.as_bytes());
    if hash.len() > 64 {
        hash.truncate(64);
    }

    // 2. 提取后缀
    let ext = Path::new(url)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("jpg"); // 默认 jpg

    let ext = if let Some(idx) = ext.find('?') {
        &ext[..idx]
    } else {
        ext
    };

    format!("{}.{}", hash, ext)
}

pub async fn file_exists(path: impl AsRef<Path>) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

pub async fn save_file(path: impl AsRef<Path>, data: &[u8]) -> std::io::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, data).await?;
    Ok(())
}
