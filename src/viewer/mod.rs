//! 代码页的查看器（v1.8）：图片 / GIF / 原始像素、Markdown 预览、JSON / YAML / TOML 树、CSV 表格和曲线。
//! 和 binview 里的 HEX / ELF / map 查看器一样，挂在标签页的「特殊查看器」上，可以和原文切换。

pub mod csv;
pub mod image;
pub mod md;
pub mod tree;
