//! HTML pages: templates and their compiled scripts from `web/` (TypeScript,
//! built to `web/dist/` and committed, so building umstieg needs no Node),
//! with JSON data embedded.

use anyhow::{Context, Result};
use serde::Serialize;

pub const PLOT: Page = Page { html: include_str!("../web/plot.html"), script: include_str!("../web/dist/plot.js") };
pub const FORM: Page = Page { html: include_str!("../web/form.html"), script: include_str!("../web/dist/form.js") };
pub const PROGRESS: Page =
    Page { html: include_str!("../web/progress.html"), script: include_str!("../web/dist/progress.js") };

const DATA: &str = "/*__DATA__*/";
const SCRIPT: &str = "/*__SCRIPT__*/";

pub struct Page {
    html: &'static str,
    script: &'static str,
}

impl Page {
    /// The page with `data` as JSON at `/*__DATA__*/` and its script at
    /// `/*__SCRIPT__*/`, which comes after the data in every template.
    pub fn render(&self, data: Option<&impl Serialize>) -> Result<String> {
        // Placeholders are only looked for in the template: data or script
        // text that looks like one stays as it is.
        let (top, bottom) = self.html.split_once(SCRIPT).context("page template without script placeholder")?;
        let top = match data {
            // `<` only occurs in JSON strings, where `\u003c` means the same;
            // escaped, the data can't close or confuse the surrounding <script>.
            Some(data) => top.replacen(DATA, &serde_json::to_string(data)?.replace('<', "\\u003c"), 1),
            None => top.to_owned(),
        };
        Ok(format!("{top}{}{bottom}", self.script))
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
