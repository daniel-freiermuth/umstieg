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
/// Where the shared look goes: the icon and stylesheet in the head, the logo
/// with the name (a link to a new search) in the page.
const HEAD: &str = "<!--__HEAD__-->";
const BRAND: &str = "<!--__BRAND__-->";
const THEME: &str = include_str!("../web/theme.css");
const LOGO: &str = include_str!("../web/logo.svg");

/// The logo as the page's icon, and the shared stylesheet.
fn head() -> String {
    // In a data URL `#` would start the fragment; single quotes keep the attribute intact.
    let icon = LOGO.lines().map(str::trim).collect::<String>().replace('"', "'").replace('#', "%23");
    format!("<link rel=\"icon\" href=\"data:image/svg+xml,{icon}\" />\n<style>\n{THEME}</style>")
}

/// The logo and name, linking to a new search.
fn brand() -> String {
    let logo = LOGO.trim().replacen("<svg ", "<svg aria-hidden=\"true\" ", 1);
    format!(
        "<a class=\"brand\" href=\"/\" title=\"New search\">{logo}<span class=\"word\">um<span>stieg</span></span></a>"
    )
}

pub struct Page {
    html: &'static str,
    script: &'static str,
}

impl Page {
    /// The page with the shared look at `<!--__HEAD__-->` and `<!--__BRAND__-->`,
    /// `data` as JSON at `/*__DATA__*/` and its script at `/*__SCRIPT__*/`,
    /// which comes after the rest in every template.
    pub fn render(&self, data: Option<&impl Serialize>) -> Result<String> {
        // Placeholders are only looked for in the template: data or script
        // text that looks like one stays as it is.
        let (top, bottom) = self.html.split_once(SCRIPT).context("page template without script placeholder")?;
        let top = top.replacen(HEAD, &head(), 1).replacen(BRAND, &brand(), 1);
        let top = match data {
            // `<` only occurs in JSON strings, where `\u003c` means the same;
            // escaped, the data can't close or confuse the surrounding <script>.
            Some(data) => top.replacen(DATA, &serde_json::to_string(data)?.replace('<', "\\u003c"), 1),
            None => top,
        };
        Ok(format!("{top}{}{bottom}", self.script))
    }
}

#[cfg(test)]
#[path = "page_tests.rs"]
mod tests;
