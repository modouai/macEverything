//! 条目结构:定长 24B,存双 arena 各自偏移 + 父指针 + 标志位(ADR-2)。

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub display_start: u32,
    pub display_len: u32,
    pub match_start: u32,
    pub match_len: u32,
    pub parent: u32,
    pub flags: u8,
}

pub const FLAG_DIR: u8 = 1;
pub const FLAG_TOMBSTONE: u8 = 2;
pub const NO_PARENT: u32 = u32::MAX;

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.flags & FLAG_DIR != 0
    }
    pub fn is_live(&self) -> bool {
        self.flags & FLAG_TOMBSTONE == 0
    }
}

/// 匹配形态:NFC 归一 + 全 Unicode 小写折叠(std to_lowercase,覆盖 é/É)。
pub fn normalize_for_match(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfc().collect::<String>().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_24_bytes() {
        assert_eq!(std::mem::size_of::<Entry>(), 24);
    }

    #[test]
    fn normalize_nfc_and_casefold() {
        // NFD 形态的 é(65 + U+0301)与 NFC 形态(é)折叠后一致;É→é 大小写互搜
        let a = normalize_for_match("cafe\u{301}z");
        let b = normalize_for_match("caféz");
        let c = normalize_for_match("CAFÉZ");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }
}
