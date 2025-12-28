use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::utils::generate_filename;

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
    pub fn cover_filename(&self) -> Option<String> {
        self.cover_url.as_ref().map(|url| generate_filename(url))
    }
}

/// 章节信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub index: u32,
    pub id: String,
    pub title: String,
    pub url: String,
}

impl Chapter {
    pub fn filename(&self) -> String {
        format!("chap_{}", self.id)
    }
}

/// 卷信息
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

    pub fn into_chapters(self) -> impl Iterator<Item = Chapter> {
        match self {
            BookItem::Chapter(c) => vec![c].into_iter(),
            BookItem::Volume(v) => v.chapters.into_iter().collect::<Vec<_>>().into_iter(),
        }
    }
}

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

    pub fn unique_id(&self) -> String {
        format!("{}_{}", self.site_id, self.id)
    }

    pub fn into_chapters(self) -> impl Iterator<Item = Chapter> {
        self.items.into_iter().flat_map(BookItem::into_chapters)
    }

    pub fn chapters(&self) -> impl Iterator<Item = Chapter> + '_ {
        self.items.iter().cloned().flat_map(BookItem::into_chapters)
    }

    pub fn chapter_count(&self) -> usize {
        self.items
            .iter()
            .map(|item| match item {
                BookItem::Chapter(_) => 1,
                BookItem::Volume(v) => v.chapters.len(),
            })
            .sum()
    }

    pub async fn work_dir(&self) -> PathBuf {
        let dir = self
            .base_dir
            .join("book")
            .join(&self.site_id)
            .join(&self.id);
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }

    pub async fn text_dir(&self) -> PathBuf {
        let work_dir = self.work_dir().await;
        let dir = work_dir.join("Text");
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }

    pub async fn cover_dir(&self) -> PathBuf {
        let work_dir = self.work_dir().await;
        let dir = work_dir.join("Cover");
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }

    pub async fn images_dir(&self) -> PathBuf {
        let work_dir = self.work_dir().await;
        let dir = work_dir.join("Images");
        tokio::fs::create_dir_all(&dir).await.ok();
        dir
    }
}
