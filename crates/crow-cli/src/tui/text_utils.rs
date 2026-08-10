use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub fn text_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate_to_width(text: &str, max_width: usize) -> String {
    if text_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width >= max_width {
            out.push('…');
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_by_display_width() {
        assert_eq!(truncate_to_width("abcdef", 4), "abc…");
        assert_eq!(truncate_to_width("你好世界", 4), "你…");
        assert_eq!(text_width("a🙂"), 3);
    }
}
