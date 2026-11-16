//! Skill discovery and on-demand loading for `SKILL.md` files.
//!
//! Standard library only. See `README.md` for the layer boundary and the
//! contract this crate mirrors.

mod error;
mod frontmatter;
mod fsio;
mod scanner;
mod util;

pub use crate::error::{Error, ErrorKind, Result};
pub use crate::scanner::{scan, Scan, Set, Skill, SkillError, SkillErrorKind};
