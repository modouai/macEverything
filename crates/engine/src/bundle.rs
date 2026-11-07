//! bundle 判定：扩展名清单与后缀匹配（查询排序与包内剪枝共用）。

pub const BUNDLE_EXTENSIONS: [&str; 9] = [
    ".app",
    ".framework",
    ".bundle",
    ".plugin",
    ".appex",
    ".xpc",
    ".kext",
    ".prefpane",
    ".qlgenerator",
];

/// 条目名是否以 bundle 扩展名结尾（ASCII 大小写不敏感）。
pub fn is_bundle_name(display: &[u8]) -> bool {
    BUNDLE_EXTENSIONS.iter().any(|ext| {
        display.len() >= ext.len()
            && display[display.len() - ext.len()..].eq_ignore_ascii_case(ext.as_bytes())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 清单逐项命中，大小写不敏感。
    /// 期望值独立于被测常量硬编码：清单本身是契约，自引用断言会让拼写错误恒绿。
    #[test]
    fn bundle_extensions_match_case_insensitively() {
        const EXPECTED: [&str; 9] = [
            ".app",
            ".framework",
            ".bundle",
            ".plugin",
            ".appex",
            ".xpc",
            ".kext",
            ".prefpane",
            ".qlgenerator",
        ];
        assert_eq!(BUNDLE_EXTENSIONS, EXPECTED, "扩展名清单不得漂移");
        for ext in EXPECTED {
            let lower = format!("X{ext}");
            let upper = format!("X{}", ext.to_ascii_uppercase());
            assert!(is_bundle_name(lower.as_bytes()), "{lower}");
            assert!(is_bundle_name(upper.as_bytes()), "{upper}");
        }
    }

    /// 后缀必须完整匹配：`.app` 子串、中文后缀、无点形态均不命中。
    #[test]
    fn non_bundle_names_do_not_match() {
        for name in [
            "notes.app.txt",
            "应用.app备份",
            "Contents",
            ".application",
            "app",
            "X.apps",
        ] {
            assert!(!is_bundle_name(name.as_bytes()), "{name}");
        }
        // 边界：空名、短于最短扩展名、差一个字符均不命中；裸扩展名本身命中
        // （名为 ".app" 的目录确实以该扩展名结尾，语义按 bundle 处理）。
        assert!(!is_bundle_name(b""), "空名");
        assert!(!is_bundle_name(b"a"), "短于最短扩展名");
        assert!(!is_bundle_name(b".ap"), "差一个字符");
        assert!(is_bundle_name(b".app"), "裸扩展名本身");
    }
}
