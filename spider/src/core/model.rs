//! 领域模型定义 (Domain Models)
//!
//! 抽象采集任务中的实体对象，包括元数据、资源分层及物理存储映射。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::utils::generate_filename;

/// 书籍元数据 (Metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metadata {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<String>,
}

fn default_language() -> String {
    "zh".to_string()
}

impl Metadata {
    /// 封面文件名序列化
    pub fn cover_filename(&self) -> Option<String> {
        self.cover_url.as_ref().map(|url| generate_filename(url))
    }
}

/// 章节实体 (Chapter Entity)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub index: u32,
    pub id: String,
    pub title: String,
    pub url: String,
}

impl Chapter {
    /// 物理文件命名规则
    pub fn filename(&self) -> String {
        format!("chap_{}", self.id)
    }
}

/// 卷/合集实体 (Volume Entity)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Volume {
    pub index: u32,
    pub id: String,
    pub title: String,
    pub cover_url: Option<String>,
    pub description: Option<String>,
    pub chapters: Vec<Chapter>,
}

impl Volume {
    pub fn cover_filename(&self) -> Option<String> {
        self.cover_url.as_ref().map(|url| generate_filename(url))
    }

    /// 生成卷索引页 HTML 片段
    pub fn content(&self) -> String {
        let mut content = String::new();

        if let Some(cover) = &self.cover_filename() {
            let cover_tag = format!(
                r#"<div class="cover"><img src="../Images/{}" alt="Cover"/></div>"#,
                cover
            );
            content.push_str(&cover_tag);
        }

        if let Some(desc) = &self.description {
            let desc_tag = format!(r#"<div class="description">{}</div>"#, desc);
            content.push_str(&desc_tag);
        }

        content
    }
}

/// 资源项枚举 (Resource Item Variant)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BookItem {
    Chapter(Chapter),
    Volume(Volume),
}

impl BookItem {
    pub fn index(&self) -> u32 {
        match self {
            BookItem::Chapter(c) => c.index,
            BookItem::Volume(v) => v.index,
        }
    }

    /// 将项平展为章节迭代器
    pub fn into_chapters(self) -> impl Iterator<Item = Chapter> {
        match self {
            BookItem::Chapter(c) => vec![c].into_iter(),
            BookItem::Volume(v) => v.chapters.into_iter().collect::<Vec<_>>().into_iter(),
        }
    }
}

/// 书籍根实体 (Aggregate Root)
#[derive(Debug, Clone)]
pub struct Book {
    pub site_id: String,
    pub id: String,
    pub metadata: Metadata,
    pub items: Vec<BookItem>,
    pub base_dir: PathBuf,
}

impl Book {
    pub fn new(
        site_id: String,
        id: String,
        metadata: Metadata,
        items: Vec<BookItem>,
        base_dir: PathBuf,
    ) -> Self {
        Self {
            site_id,
            id,
            metadata,
            items,
            base_dir,
        }
    }

    /// 跨站点全局唯一标识
    pub fn unique_id(&self) -> String {
        format!("{}_{}", self.site_id, self.id)
    }

    pub fn into_chapters(self) -> impl Iterator<Item = Chapter> {
        self.items.into_iter().flat_map(BookItem::into_chapters)
    }

    pub fn chapters(&self) -> impl Iterator<Item = Chapter> + '_ {
        self.items.iter().cloned().flat_map(BookItem::into_chapters)
    }

    /// 获取展平后的总章节数
    pub fn chapter_count(&self) -> usize {
        self.items
            .iter()
            .map(|item| match item {
                BookItem::Chapter(_) => 1,
                BookItem::Volume(v) => v.chapters.len(),
            })
            .sum()
    }

    /// 获取工作根目录并确保其物理存在
    pub async fn work_dir(&self) -> PathBuf {
        let dir = self
            .base_dir
            .join("book")
            .join(&self.site_id)
            .join(&self.id);
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }

    /// 获取文本存储目录
    pub async fn text_dir(&self) -> PathBuf {
        let work_dir = self.work_dir().await;
        let dir = work_dir.join("Text");
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }

    /// 获取封面存储目录
    pub async fn cover_dir(&self) -> PathBuf {
        let work_dir = self.work_dir().await;
        let dir = work_dir.join("Cover");
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }

    /// 获取通用资源存储目录
    pub async fn images_dir(&self) -> PathBuf {
        let work_dir = self.work_dir().await;
        let dir = work_dir.join("Images");
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }
}
