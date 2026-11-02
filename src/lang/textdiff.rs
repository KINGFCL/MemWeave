//! 逐字比较两段文字，用在 `comparison display-diff` 上。
//!
//! 算法用 `dissimilar`：Myers 加语义整理，是 Google diff-match-patch 的移植。记忆里的文字短，而且
//! 中文的字符很容易碰巧相同，普通的 Myers 会把无关的字硬对齐，切成读不懂的碎片，语义整理才决定好不好读。

/// 去掉首尾相同的部分后，超过这个大小就不逐字比较。`dissimilar` 没有超时，完全不同的文字耗时增长很快：
/// 6 KB 要 26 ms，60 KB 要 2.6 秒，1 MiB 超过 15 秒。图是单线程的，卡住时所有调用都要排队。
pub const MAX_COMPARED_BYTES: usize = 8 * 1024;

/// 比较结果的一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part<'a> {
    Same(&'a str),
    Removed(&'a str),
    Added(&'a str),
}

/// 把 `old` 变成 `new` 要改哪些地方。改动太大、比较太慢时返回 `None`。
pub fn compare<'a>(old: &'a str, new: &'a str) -> Option<Vec<Part<'a>>> {
    let prefix = common_prefix(old, new);
    let suffix = common_suffix(&old[prefix..], &new[prefix..]);
    if old.len() - prefix - suffix > MAX_COMPARED_BYTES
        || new.len() - prefix - suffix > MAX_COMPARED_BYTES
    {
        return None;
    }
    Some(
        dissimilar::diff(old, new)
            .into_iter()
            .map(|chunk| match chunk {
                dissimilar::Chunk::Equal(text) => Part::Same(text),
                dissimilar::Chunk::Delete(text) => Part::Removed(text),
                dissimilar::Chunk::Insert(text) => Part::Added(text),
            })
            .collect(),
    )
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.chars()
        .zip(b.chars())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

fn common_suffix(a: &str, b: &str) -> usize {
    a.chars()
        .rev()
        .zip(b.chars().rev())
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len_utf8())
        .sum()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// 比较结果必须能还原出两边的原文，否则模型看到的就是错的。
    fn assert_rebuilds(old: &str, new: &str) -> Vec<String> {
        let parts = compare(old, new).expect("这些文字不算长");
        let (mut left, mut right) = (String::new(), String::new());
        for part in &parts {
            match part {
                Part::Same(text) => {
                    left.push_str(text);
                    right.push_str(text);
                }
                Part::Removed(text) => left.push_str(text),
                Part::Added(text) => right.push_str(text),
            }
        }
        assert_eq!(left, old);
        assert_eq!(right, new);
        parts
            .iter()
            .filter_map(|part| match part {
                Part::Same(_) => None,
                Part::Removed(text) => Some(format!("-{text}")),
                Part::Added(text) => Some(format!("+{text}")),
            })
            .collect()
    }

    #[test]
    fn finds_small_changes_in_chinese() {
        assert_eq!(
            assert_rebuilds(
                "知识关系归 AI。写操作进 journal。",
                "知识关系归 AI。save 时才写进 journal。"
            ),
            ["-写操作", "+save 时才写"]
        );
    }

    #[test]
    fn a_heavy_rewrite_stays_readable() {
        // 这是记忆日志里真实的一次改写。不能切成碰巧相同的单字。
        let changes = assert_rebuilds(
            "日志写到一半时，下次启动能跳过半行，但下一次写入会接在半行后面，再重启就报损坏。半行切在汉字中间时，重启直接打不开。这是 0.1.0-alpha.2 的已知缺陷。",
            "0.1.0-alpha.2 的缺陷：日志写到一半时，下一次写入会接在半行后面，再重启就报损坏。已在 fix/stability 分支的 6707924 修复：追加前先去掉半行，写失败时回退。",
        );
        assert!(changes.len() <= 2, "{changes:?}");
    }

    #[test]
    fn handles_the_edges() {
        assert_eq!(assert_rebuilds("", "新"), ["+新"]);
        assert_eq!(assert_rebuilds("旧", ""), ["-旧"]);
        assert!(assert_rebuilds("", "").is_empty());
        assert!(assert_rebuilds("一样", "一样").is_empty());
        // 字符有相同的字节，不能切在字符中间。
        assert_rebuilds("汉", "汊");
        assert_rebuilds("a🎉b", "a🎊b");
        assert_rebuilds("👨‍👩‍👧", "👨‍👩‍👦");
        assert_rebuilds("一\n二\n", "一\r\n二\n");
    }

    #[test]
    fn random_texts_always_rebuild() {
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |limit: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as usize % limit
        };
        let alphabet: Vec<char> = "ab 甲乙丙，。🎉\n\"\\".chars().collect();
        let mut text = |length: usize| -> String {
            (0..length)
                .map(|_| alphabet[next(alphabet.len())])
                .collect()
        };
        for round in 0..2000 {
            let old = text(round % 80);
            let new = text((round * 7) % 80);
            assert_rebuilds(&old, &new);
        }
    }

    fn scattered(length: usize, seed: u64) -> String {
        let mut state = seed;
        (0..length)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                char::from_u32(0x4e00 + (state % 20000) as u32).unwrap()
            })
            .collect()
    }

    #[test]
    fn a_long_rewrite_is_not_compared() {
        let (old, new) = (scattered(5000, 1), scattered(5000, 2));
        assert_eq!(compare(&old, &new), None);
        // 大小只看变化的部分，首尾相同的部分再长也没关系。
        let same = scattered(300_000, 3);
        let (old, new) = (format!("{same}旧{same}"), format!("{same}新{same}"));
        assert_eq!(compare(&old, &new).unwrap().len(), 4);
    }

    #[test]
    fn the_largest_compared_text_stays_fast() {
        // 调试版比发布版慢得多，这里只防止量级出错。
        let (old, new) = (scattered(2700, 1), scattered(2700, 2));
        assert!(old.len() <= MAX_COMPARED_BYTES && new.len() <= MAX_COMPARED_BYTES);
        let start = Instant::now();
        compare(&old, &new).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
    }
}
