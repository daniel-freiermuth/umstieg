use serde_json::json;

use super::{FORM, PLOT, PROGRESS, Page};
use crate::testkit::embedded_json;

const TEMPLATE: Page = Page {
    html: "<p>/*__DATA__*/ outside</p><script id=\"data\" type=\"application/json\">/*__DATA__*/</script>\
           <script>/*__SCRIPT__*/</script><p>/*__SCRIPT__*/ after</p>",
    // A script mentioning the data placeholder must not receive the data.
    script: "const marker = '/*__DATA__*/';",
};

#[test]
fn data_fills_the_first_data_placeholder_and_the_script_the_first_script_placeholder() {
    let page = TEMPLATE.render(Some(&json!({"a": 1}))).unwrap();
    assert_eq!(
        page,
        "<p>{\"a\":1} outside</p><script id=\"data\" type=\"application/json\">/*__DATA__*/</script>\
         <script>const marker = '/*__DATA__*/';</script><p>/*__SCRIPT__*/ after</p>"
    );
}

#[test]
fn data_text_survives_verbatim_and_cannot_break_out_of_its_script() {
    let page = Page {
        html: "<script id=\"data\" type=\"application/json\">/*__DATA__*/</script><script>/*__SCRIPT__*/</script>",
        script: "run();",
    };
    let nasty = [
        "</script><script>alert(1)</script>",
        "</SCRIPT>",
        "<!--<script>",
        "/*__SCRIPT__*/",
        "/*__DATA__*/",
        "a<b & c>d",
    ];
    for text in nasty {
        let data = json!({ "name": text });
        let html = page.render(Some(&data)).unwrap();
        // The page still has exactly its own two scripts, the code once, at the end.
        assert_eq!(html.matches("<script").count(), 2, "{text}: {html}");
        assert_eq!(html.to_ascii_lowercase().matches("</script>").count(), 2, "{text}: {html}");
        assert_eq!(html.matches("run();").count(), 1, "{text}: {html}");
        assert!(html.ends_with("<script>run();</script>"), "{text}: {html}");
        assert_eq!(embedded_json(&html, "data"), data, "{text}");
    }
}

#[test]
fn without_data_only_the_script_is_filled() {
    let html = PROGRESS.render(None::<&()>).unwrap();
    assert!(!html.contains("/*__SCRIPT__*/"));
    assert!(html.contains(PROGRESS.script));
}

#[test]
fn real_templates_embed_data_before_their_script() {
    for (page, id) in [(PLOT, "data"), (FORM, "spec")] {
        let data = json!({ "stations": ["Mannheim Hbf", "</script>"] });
        let html = page.render(Some(&data)).unwrap();
        assert_eq!(embedded_json(&html, id), data);
        assert!(!html.contains("/*__DATA__*/") && !html.contains("/*__SCRIPT__*/"));
        let script_at = html.find(page.script).unwrap();
        assert!(html.find("Mannheim Hbf").unwrap() < script_at);
        assert_eq!(html.matches(page.script).count(), 1);
    }
}
