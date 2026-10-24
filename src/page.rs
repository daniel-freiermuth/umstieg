//! HTML pages: templates and their compiled scripts from `web/` (TypeScript,
//! built to `web/dist/` and committed, so building umstieg needs no Node),
//! with JSON data embedded.

use anyhow::Result;
use serde::Serialize;

pub const PLOT: Page = Page { html: include_str!("../web/plot.html"), script: include_str!("../web/dist/plot.js") };
pub const FORM: Page = Page { html: include_str!("../web/form.html"), script: include_str!("../web/dist/form.js") };
pub const PROGRESS: Page =
    Page { html: include_str!("../web/progress.html"), script: include_str!("../web/dist/progress.js") };

pub struct Page {
    html: &'static str,
    script: &'static str,
}

impl Page {
    /// The page with `data` as JSON at `/*__DATA__*/` and its script.
    pub fn render(&self, data: Option<&impl Serialize>) -> Result<String> {
        let page = match data {
            // Inline JSON must not close the surrounding <script>.
            Some(data) => self.html.replacen("/*__DATA__*/", &serde_json::to_string(data)?.replace("</", "<\\/"), 1),
            None => self.html.to_owned(),
        };
        // Last, so that text in the data can't be taken for the placeholder.
        Ok(page.replacen("/*__SCRIPT__*/", self.script, 1))
    }
}
