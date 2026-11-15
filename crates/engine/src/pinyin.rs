//! 拼音查询展开:读音归一、音节切分、反查表与位图序列匹配。
//! 纯查询侧展开,索引/内存布局/落盘格式零改动(设计见
//! docs/superpowers/specs/2026-11-15-pinyin-search-design.md)。
use pinyin::ToPinyinMulti;

/// 单字的归一化读音集:去重、去声调、`ü` 写作 `v`(主流 IME 输入习惯)。
/// 上游 `plain()` 对 j/q/x/y 后的 ü 已写作 u(居 ju、去 qu、学 xue、鱼 yu),
/// 只有 lü/nü 保留 `ü`,这里统一转 v(绿 lv、女 nv)。
#[cfg_attr(not(test), expect(dead_code))] // 反查表接入前仅测试使用;接入后 expect 会提示移除本属性
fn readings(c: char) -> Vec<String> {
    let Some(multi) = c.to_pinyin_multi() else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for p in multi {
        let r = p.plain().replace('ü', "v");
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_strip_tones_and_fold_umlaut() {
        assert_eq!(readings('绿'), vec!["lv", "lu"]);
        assert_eq!(readings('女')[0], "nv");
        assert_eq!(readings('居'), vec!["ju", "ji"]);
    }

    #[test]
    fn polyphonic_chars_keep_all_readings() {
        assert_eq!(readings('长'), vec!["zhang", "chang"]);
        let xing = readings('行');
        assert!(xing.iter().any(|r| r == "xing"), "{xing:?}");
        assert!(xing.iter().any(|r| r == "hang"), "{xing:?}");
        assert_eq!(readings('数'), vec!["shu", "shuo"]);
    }

    #[test]
    fn cjk_basic_block_coverage_is_near_complete() {
        let covered = (0x4E00u32..=0x9FFF)
            .filter_map(char::from_u32)
            .filter(|&c| !readings(c).is_empty())
            .count();
        assert!(covered > 20_000, "covered={covered}");
    }
}
