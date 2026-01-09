//! Booktoki 选择器
//!
//! 预编译的 CSS 选择器

use std::sync::OnceLock;

use scraper::Selector;

/// 站点选择器集合
pub struct SiteSelectors {
    pub detail_desc: Selector,
    pub view_content: Selector,
    pub view_img: Selector,
    pub bold: Selector,
    pub icon_user: Selector,
    pub icon_tags: Selector,
    pub icon_building: Selector,
    pub wr_none: Selector,
    pub list_item: Selector,
    pub wr_subject_link: Selector,
    pub wr_num: Selector,
    pub pagination_next: Selector,
    pub novel_content: Selector,
    pub paragraph: Selector,
}

static SELECTORS: OnceLock<SiteSelectors> = OnceLock::new();

impl SiteSelectors {
    /// 获取全局选择器实例
    pub fn get() -> &'static SiteSelectors {
        SELECTORS.get_or_init(|| SiteSelectors {
            detail_desc: Selector::parse("div[itemprop='description']").unwrap(),
            view_content: Selector::parse("div.view-content").unwrap(),
            view_img: Selector::parse("div.view-img img").unwrap(),
            bold: Selector::parse("b").unwrap(),
            icon_user: Selector::parse("i.fa-user").unwrap(),
            icon_tags: Selector::parse("i.fa-tag").unwrap(),
            icon_building: Selector::parse("i.fa-building-o").unwrap(),
            wr_none: Selector::parse("div.wr-none").unwrap(),
            list_item: Selector::parse("ul.list-body > li.list-item").unwrap(),
            wr_subject_link: Selector::parse("div.wr-subject > a").unwrap(),
            wr_num: Selector::parse("div.wr-num").unwrap(),
            pagination_next: Selector::parse("ul.pagination li.active + li a[href]").unwrap(),
            novel_content: Selector::parse("div#novel_content").unwrap(),
            paragraph: Selector::parse("p").unwrap(),
        })
    }
}
