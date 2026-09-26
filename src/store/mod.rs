//! 落盘层（spec §9.2/§9.3）。
//!
//! 本模块与 `apply` 是全项目**仅有的**两个允许调写 API 的目录（§9.9 白名单）。
//! `store` 回答「内容是什么、谁交付的」，`apply` 回答「往哪写」——分开是为了让
//! 「决定交什么」这件事可以被纯函数测出来，而不必真的落盘。

pub mod hash;
pub mod provenance;
pub mod skills;
