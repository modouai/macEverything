//! 追加式名称池:写入返回 (start, len);只增不删(删除=墓碑),整理时重建。

#[derive(Default)]
pub struct ArenaBuilder {
    pub data: Vec<u8>,
}

impl ArenaBuilder {
    pub fn push(&mut self, bytes: &[u8]) -> (u32, u32) {
        let start = self.data.len() as u32;
        self.data.extend_from_slice(bytes);
        (start, bytes.len() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_returns_offset() {
        let mut a = ArenaBuilder::default();
        let (s1, l1) = a.push(b"abc");
        let (s2, l2) = a.push(b"de");
        assert_eq!((s1, l1), (0, 3));
        assert_eq!((s2, l2), (3, 2));
        assert_eq!(&a.data[0..3], b"abc");
    }
}
