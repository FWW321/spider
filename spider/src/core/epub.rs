//! EPUB 编译引擎 (EPUB Compilation Engine)
//!
//! 负责将领域模型 (Book/Chapter) 序列化为符合 IDPF 规范的 EPUB 容器 (OCF 结构)。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use epub_builder::{EpubBuilder, EpubContent, ZipLibrary};
use mime_guess::MimeGuess;
use tokio::fs;

use crate::core::model::{Book, BookItem, Chapter, Volume};

/// EPUB 文档生成器
pub struct EpubGenerator {
    /// 待编译的书籍聚合模型
    book: Book,
    /// 内部文本资源路径映射
    chapter_dir: String,
    /// 内部静态资源路径映射
    image_dir: String,
}

impl EpubGenerator {
    pub fn new(book: Book) -> Self {
        Self {
            book,
            chapter_dir: "Text".to_string(),
            image_dir: "Images".to_string(),
        }
    }

    /// 执行文档编译流水线
    /// 
    /// 元数据注入、目录树构建、静态资源打包及最终 ZIP 压缩。
    pub async fn run<P: AsRef<Path>>(&self, output_path: Option<P>) -> Result<PathBuf> {
        let mut builder = EpubBuilder::new(ZipLibrary::new().map_err(|e| anyhow::anyhow!(e))?)
            .map_err(|e| anyhow::anyhow!(e))?;

        self.configure_metadata(&mut builder).await?;
        self.build_structure(&mut builder).await?;
        self.add_image_items(&mut builder).await?;

        let final_path = match output_path {
            Some(p) => p.as_ref().to_path_buf(),
            None => PathBuf::from(format!("{}.epub", self.book.id)),
        };

        let final_path_clone = final_path.clone();
        // 将 CPU 密集型任务 (ZIP 压缩) 卸载至专用线程池 (Task Offloading)
        tokio::task::spawn_blocking(move || -> Result<()> {
            let file = std::fs::File::create(&final_path_clone)
                .with_context(|| format!("Failed to create artifact: {:?}", final_path_clone))?;
            builder.generate(file).map_err(|e| anyhow::anyhow!(e))?;
            Ok(())
        })
        .await
        .map_err(|e| anyhow::anyhow!("Worker join error: {}", e))??;

        Ok(final_path)
    }

    /// 注入书籍元数据 (Dublin Core Metadata)
    async fn configure_metadata(&self, builder: &mut EpubBuilder<ZipLibrary>) -> Result<()> {
        let meta = &self.book.metadata;

        builder.set_title(&meta.title);

        if let Some(author) = &meta.author {
            builder.add_author(author);
        }

        if let Some(summary) = &meta.summary {
            builder.set_description(vec![summary.to_string()]);
        }

        builder.set_lang(&meta.language);

        for tag in &meta.tags {
            builder.add_subject(tag);
        }

        // 封面资源嵌入与元数据关联
        if let Some(cover_name) = meta.cover_filename() {
            let cover_path = self.book.cover_dir().await.join(&cover_name);
            if cover_path.exists() {
                let content = fs::read(&cover_path).await?;
                let mime = MimeGuess::from_path(&cover_path)
                    .first_raw()
                    .unwrap_or("image/jpeg");
                builder
                    .add_cover_image(&cover_name, content.as_slice(), mime)
                    .map_err(|e| anyhow::anyhow!(e))?;
            }
        }

        Ok(())
    }

    /// 递归构建文档结构树 (TOC Structure)
    async fn build_structure(&self, builder: &mut EpubBuilder<ZipLibrary>) -> Result<()> {
        for item in &self.book.items {
            match item {
                BookItem::Chapter(chapter) => {
                    self.add_chapter(builder, chapter, None).await?;
                }
                BookItem::Volume(volume) => {
                    self.add_volume(builder, volume).await?;
                }
            }
        }
        Ok(())
    }

    /// 添加章节实体并执行 HTML 内容包装
    async fn add_chapter(
        &self,
        builder: &mut EpubBuilder<ZipLibrary>,
        chapter: &Chapter,
        parent_level: Option<i32>,
    ) -> Result<String> {
        let file_name = format!("{}/{}.xhtml", self.chapter_dir, chapter.filename());
        let text_dir = self.book.text_dir().await;
        let content_path = text_dir.join(chapter.filename());

        let content = if content_path.exists() {
            let content = fs::read_to_string(&content_path).await?;
            format!(
                "<h1>{}</h1><div id=\"content\">{}</div>",
                chapter.title, content
            )
        } else {
            format!("<h1>{}</h1><div id=\"content\"></div>", chapter.title)
        };

        let xhtml_content = self.wrap_html(&chapter.title, &content);

        let mut epub_content =
            EpubContent::new(&file_name, xhtml_content.as_bytes()).title(&chapter.title);

        if let Some(level) = parent_level {
            epub_content = epub_content.level(level + 1);
        }

        builder
            .add_content(epub_content)
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(file_name)
    }

    /// 添加卷索引及其嵌套层级
    async fn add_volume(
        &self,
        builder: &mut EpubBuilder<ZipLibrary>,
        volume: &Volume,
    ) -> Result<()> {
        let volume_title = &volume.title;
        let volume_content = {
            let content = volume.content();
            format!(
                "<h1>{}</h1><div id=\"content\">{}</div>",
                volume_title, content
            )
        };

        let file_name = format!("{}/volume_{}.xhtml", self.chapter_dir, volume.id);
        let xhtml_content = self.wrap_html(volume_title, &volume_content);

        builder
            .add_content(
                EpubContent::new(&file_name, xhtml_content.as_bytes())
                    .title(volume_title)
                    .level(1),
            )
            .map_err(|e| anyhow::anyhow!(e))?;

        for chapter in &volume.chapters {
            self.add_chapter(builder, chapter, Some(1)).await?;
        }

        Ok(())
    }

    /// 批量打包静态媒体资源
    async fn add_image_items(&self, builder: &mut EpubBuilder<ZipLibrary>) -> Result<()> {
        let images_dir = self.book.images_dir().await;
        if !images_dir.exists() {
            return Ok(());
        }

        let mut entries = fs::read_dir(images_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.is_file() {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                let dest_path = format!("{}/{}", self.image_dir, file_name);
                let content = fs::read(&path).await?;
                let mime = MimeGuess::from_path(&path)
                    .first_raw()
                    .unwrap_or("image/jpeg");

                builder
                    .add_resource(dest_path, content.as_slice(), mime)
                    .map_err(|e| anyhow::anyhow!(e))?;
            }
        }
        Ok(())
    }

    /// 应用 XHTML 1.1 标准模板包装
    fn wrap_html(&self, title: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.1//EN" "http://www.w3.org/TR/xhtml11/DTD/xhtml11.dtd">
<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="{}">
<head>
    <meta http-equiv="Content-Type" content="application/xhtml+xml; charset=utf-8" />
    <title>{}</title>
</head>
<body> 
{}
</body>
</html>"#,
            self.book.metadata.language, title, body
        )
    }
}
