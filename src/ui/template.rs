//! `{placeholder}` templates for the status bar and window title.
//!
//! A template is split into segments at runs of two or more spaces. A segment whose
//! placeholders are all known but one of them is empty drops out with its separator, so
//! `{format}  {width}×{height}  {index}/{count}` shows just `012/240` in the grid. Unknown
//! names stay as written.

/// Expand `tpl`, looking placeholders up with `var` (`None` = not a placeholder).
pub fn expand(tpl: &str, var: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out: Vec<String> = Vec::new();
    for seg in segments(tpl) {
        if let Some(s) = expand_segment(seg, var) {
            out.push(s);
        }
    }
    out.join("  ")
}

/// Split at runs of two or more spaces.
fn segments(tpl: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = tpl.trim();
    while let Some(i) = rest.find("  ") {
        out.push(&rest[..i]);
        rest = rest[i..].trim_start();
    }
    if !rest.is_empty() {
        out.push(rest);
    }
    out
}

/// One segment, or `None` if a placeholder in it expanded to nothing.
fn expand_segment(seg: &str, var: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(seg.len());
    let mut rest = seg;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('}') {
            Some(close) => {
                let name = &after[..close];
                match var(name) {
                    Some(v) if v.is_empty() => return None,
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('{');
                        out.push_str(name);
                        out.push('}');
                    }
                }
                rest = &after[close + 1..];
            }
            None => {
                out.push_str(&rest[open..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    (!out.is_empty()).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(name: &str) -> Option<String> {
        match name {
            "name" => Some("a.png".into()),
            "format" | "width" | "height" => Some(String::new()),
            "index" => Some("012".into()),
            "count" => Some("240".into()),
            _ => None,
        }
    }

    #[test]
    fn empty_segments_drop_out() {
        assert_eq!(
            expand("{format}  {width}×{height}  {index}/{count}", &vars),
            "012/240"
        );
        assert_eq!(expand("{name} — remage", &vars), "a.png — remage");
        // Unknown placeholders survive; single spaces don't split.
        assert_eq!(expand("{name}   {nope} x", &vars), "a.png  {nope} x");
        assert_eq!(expand("open {", &vars), "open {");
        assert_eq!(expand("", &vars), "");
    }
}
