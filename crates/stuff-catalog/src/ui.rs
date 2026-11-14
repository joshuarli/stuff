#![forbid(unsafe_code)]

//! The `ui-*` markup of `@laputa-systems/ui-dashboard` that the catalog pages
//! emit. Callers pass escaped HTML wherever a parameter is named `*_html`;
//! every other text parameter is escaped here.

const UI_TOKENS: &str = include_str!("../assets/ui-dashboard/tokens.css");
const UI_STYLES: &str = include_str!("../assets/ui-dashboard/styles.css");
const UI_LAYOUT: &str = include_str!("../assets/ui-dashboard/layout.css");
const UI_STYLES_IMPORT: &str = "@import \"./tokens.css\";\n";
const SEARCH_JS: &str = include_str!("../assets/search.js");

const APP_CSS: &str = include_str!("../assets/catalog.css");

pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escapes text and renders its Markdown code spans as `<code>`.
pub(crate) fn inline_code(text: &str) -> String {
    let mut out = String::new();
    for (index, part) in text.split('`').enumerate() {
        if index % 2 == 1 {
            out.push_str(&format!("<code>{}</code>", escape(part)));
        } else {
            out.push_str(&escape(part));
        }
    }
    out
}

pub(crate) fn count(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

fn stylesheet() -> String {
    let styles = UI_STYLES
        .strip_prefix(UI_STYLES_IMPORT)
        .expect("ui-dashboard styles.css starts by importing tokens.css");
    format!("{UI_TOKENS}\n{styles}\n{UI_LAYOUT}\n{APP_CSS}")
}

pub(crate) struct NavLink<'a> {
    pub href: &'a str,
    pub label: &'a str,
}

pub(crate) fn document(title: &str, brand: &str, nav: &[NavLink<'_>], page_html: &str) -> String {
    let links: String = nav
        .iter()
        .map(|link| format!("<a href=\"{}\">{}</a>", escape(link.href), escape(link.label)))
        .collect();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{title}</title>\n<style>{css}</style>\n</head>\n<body>\n<div class=\"ui-shell\">\n<aside class=\"ui-side\">\n<div class=\"ui-side-top\"><a class=\"ui-side-brand\" href=\"#\">{brand}</a></div>\n<div class=\"ui-side-body\"><nav class=\"ui-side-nav\" aria-label=\"Main\">{links}</nav></div>\n</aside>\n<main class=\"ui-main\" id=\"main\" tabindex=\"-1\">\n<div class=\"page\">\n{page_html}</div>\n</main>\n</div>\n<script>{SEARCH_JS}</script>\n</body>\n</html>\n",
        title = escape(title),
        css = stylesheet(),
        brand = escape(brand),
    )
}

pub(crate) fn page_header(title: &str, sub_html: &str) -> String {
    format!(
        "<header class=\"ui-page-header\"><h1>{}</h1></header>\n<p class=\"page-sub\">{sub_html}</p>\n",
        escape(title)
    )
}

pub(crate) struct Tile {
    pub label: String,
    pub value: String,
    pub note: String,
    pub href: String,
}

pub(crate) fn status_strip(tiles: &[Tile]) -> String {
    let cells: String = tiles
        .iter()
        .map(|tile| {
            format!(
                "<a class=\"tile\" href=\"{}\"><span class=\"tile-label\">{}</span><span class=\"tile-value\">{}</span><span class=\"tile-note\">{}</span></a>",
                escape(&tile.href),
                escape(&tile.label),
                escape(&tile.value),
                escape(&tile.note)
            )
        })
        .collect();
    format!("<div class=\"status-strip\" style=\"--cells:{}\">{cells}</div>\n", tiles.len().max(1))
}

pub(crate) fn panel(id: Option<&str>, title: &str, meta: &str, body_html: &str) -> String {
    let id = id.map(|id| format!(" id=\"{}\"", escape(id))).unwrap_or_default();
    let meta = if meta.is_empty() {
        String::new()
    } else {
        format!("<span class=\"ui-panel-meta\">{}</span>", escape(meta))
    };
    format!(
        "<section class=\"ui-panel\"{id}><div class=\"ui-panel-head\"><div class=\"ui-panel-title\"><h2>{}</h2></div>{meta}</div><div class=\"ui-panel-body\">{body_html}</div></section>\n",
        escape(title)
    )
}

pub(crate) fn group(title: &str, body_html: &str) -> String {
    format!(
        "<div class=\"group\"><div class=\"group-head\"><h3>{}</h3></div>{body_html}</div>",
        escape(title)
    )
}

/// One hairline row: a name with an optional note under it, and a value on the right.
pub(crate) fn row(name_html: &str, note_html: &str, value_html: &str) -> String {
    let note = if note_html.is_empty() {
        String::new()
    } else {
        format!("<span class=\"row-note\">{note_html}</span>")
    };
    let value = if value_html.is_empty() {
        String::new()
    } else {
        format!("<span class=\"row-value\">{value_html}</span>")
    };
    format!("<li><span class=\"row-name\">{name_html}{note}</span>{value}</li>")
}

/// The first `shown` rows, then the rest behind "Show N more".
pub(crate) fn short_list(rows: &[String], shown: usize) -> String {
    let head: String = rows.iter().take(shown).map(String::as_str).collect();
    let mut out = format!("<ol class=\"rows\">{head}</ol>");
    if rows.len() > shown {
        let tail: String = rows[shown..].iter().map(String::as_str).collect();
        out.push_str(&format!(
            "<details class=\"more\"><summary>Show {} more</summary><ol class=\"rows\">{tail}</ol></details>",
            count(rows.len() - shown)
        ));
    }
    out
}

pub(crate) fn snippet(code: &str) -> String {
    format!("<pre class=\"snippet\"><code>{}</code></pre>", escape(code))
}

pub(crate) fn breadcrumbs(ancestors: &[(&str, &str)], current: &str) -> String {
    let items: String = ancestors
        .iter()
        .map(|(href, label)| {
            format!(
                "<li><a href=\"{}\">{}</a><span class=\"ui-breadcrumb-separator\" aria-hidden=\"true\">/</span></li>",
                escape(href),
                escape(label)
            )
        })
        .collect();
    format!(
        "<nav class=\"ui-breadcrumbs\" aria-label=\"Breadcrumb\"><ol>{items}<li><span aria-current=\"page\">{}</span></li></ol></nav>\n",
        escape(current)
    )
}

pub(crate) const SEARCH_ICON: &str = "<svg class=\"ui-icon\" width=\"13\" height=\"13\" viewBox=\"0 0 24 24\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"1.6\" stroke-linecap=\"round\" aria-hidden=\"true\"><path d=\"M10.5 4a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13Z\"/><path d=\"m15.2 15.2 4.3 4.3\"/></svg>";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inline_search_script_cannot_close_its_own_tag() {
        assert!(!SEARCH_JS.is_empty());
        assert!(!SEARCH_JS.to_ascii_lowercase().contains("</script"));
    }

    #[test]
    fn the_stylesheet_inlines_tokens_instead_of_importing_them() {
        let css = stylesheet();
        assert!(!css.contains("@import"));
        assert!(css.contains("--ui-side-width: 236px"));
        assert!(css.contains(".ui-shell"));
        assert!(css.contains(".ui-breadcrumbs"));
        assert!(!css.to_ascii_lowercase().contains("</style"));
    }

    #[test]
    fn counts_group_thousands() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1254), "1,254");
        assert_eq!(count(1234567), "1,234,567");
        assert_eq!(plural(1, "package", "packages"), "1 package");
    }

    #[test]
    fn code_spans_are_escaped_and_marked() {
        assert_eq!(inline_code("under `current/<x>`:"), "under <code>current/&lt;x&gt;</code>:");
    }

    #[test]
    fn short_lists_hide_only_the_tail() {
        let rows: Vec<String> = (0..5).map(|i| format!("<li>{i}</li>")).collect();
        let html = short_list(&rows, 3);
        assert!(html.starts_with("<ol class=\"rows\"><li>0</li><li>1</li><li>2</li></ol>"));
        assert!(html.contains("<summary>Show 2 more</summary><ol class=\"rows\"><li>3</li><li>4</li></ol>"));
        assert!(!short_list(&rows, 5).contains("<details"));
    }
}
