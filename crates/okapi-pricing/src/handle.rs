//! PriceBook 进程内 L1：ArcSwap 无锁读 + epoch 单调热更（DESIGN §8.5）。

use crate::book::PriceBook;
use arc_swap::ArcSwap;
use std::sync::Arc;

/// 请求路径经此句柄零锁读取当前价格表；console 发布经 [`Self::swap_if_newer`] 热更。
pub struct PriceBookHandle {
    inner: ArcSwap<PriceBook>,
}

impl PriceBookHandle {
    #[must_use]
    pub fn new(initial: PriceBook) -> Self {
        Self {
            inner: ArcSwap::from_pointee(initial),
        }
    }

    /// 无锁读当前价格表（请求路径）。
    #[must_use]
    pub fn load(&self) -> Arc<PriceBook> {
        self.inner.load_full()
    }

    #[must_use]
    pub fn epoch(&self) -> i64 {
        self.inner.load().epoch()
    }

    /// 无条件原子替换（缓存清理/配置热修复通道；正常发布路径走 [`Self::swap_if_newer`]）。
    pub fn replace(&self, book: PriceBook) {
        self.inner.store(Arc::new(book));
    }

    /// 仅当 epoch 更新时原子替换（防广播乱序/重复导致回退）。返回是否发生替换。
    pub fn swap_if_newer(&self, book: PriceBook) -> bool {
        let incoming = Arc::new(book);
        let mut swapped = false;
        self.inner.rcu(|current| {
            if incoming.epoch() > current.epoch() {
                swapped = true;
                Arc::clone(&incoming)
            } else {
                swapped = false;
                Arc::clone(current)
            }
        });
        swapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book::{PriceBookSource, compile};
    use crate::error::CompileError;

    fn empty_book(epoch: i64) -> Result<PriceBook, CompileError> {
        compile(PriceBookSource {
            epoch,
            models: Vec::new(),
            groups: Vec::new(),
            overrides: Vec::new(),
            rules: Vec::new(),
        })
    }

    #[test]
    fn swaps_only_when_epoch_is_newer() -> Result<(), CompileError> {
        let handle = PriceBookHandle::new(empty_book(1)?);
        assert!(handle.swap_if_newer(empty_book(2)?));
        assert_eq!(handle.epoch(), 2);
        // 旧 epoch 与重复 epoch 均拒绝
        assert!(!handle.swap_if_newer(empty_book(1)?));
        assert!(!handle.swap_if_newer(empty_book(2)?));
        assert_eq!(handle.epoch(), 2);
        Ok(())
    }
}
