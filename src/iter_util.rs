//! Small iterator utilities shared across the crate.

/// Extension trait that turns any iterator into one yielding fixed-capacity
/// `heapless::Vec<T, N>` chunks. The final chunk may be shorter than `N`
/// if the source iterator doesn't divide evenly. The iterator terminates
/// on the first empty chunk, so an exhausted source produces no trailing
/// `Some(empty)`.
pub trait ChunksHeaplessExt: Iterator + Sized {
    fn chunks_heapless<const N: usize>(self) -> ChunksHeapless<Self, N> {
        ChunksHeapless {
            inner: self,
            stored: None,
        }
    }
}

impl<I: Iterator> ChunksHeaplessExt for I {}

pub struct ChunksHeapless<I: Iterator, const N: usize> {
    inner: I,
    stored: Option<I::Item>,
}

impl<I: Iterator, const N: usize> Iterator for ChunksHeapless<I, N> {
    type Item = heapless::Vec<I::Item, N>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut ret = heapless::Vec::new();
        if let Some(stored) = self.stored.take() {
            if let Err(stored) = ret.push(stored) {
                self.stored = Some(stored);
            }
        }
        while self.stored.is_none()
            && let Some(next) = self.inner.next()
        {
            if let Err(stored) = ret.push(next) {
                self.stored = Some(stored);
            }
        }
        (!ret.is_empty()).then_some(ret)
    }
}
