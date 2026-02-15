use std::sync::{Arc, Weak};

use ijson::IValue;

use crate::transmit::IsolateState;

#[repr(transparent)]
#[derive(Clone, Debug)]
pub struct ArcStackItem(pub Arc<(Weak<IsolateState>, IValue)>);

impl ArcStackItem {
    #[inline(always)]
    pub fn get_weak(&self) -> Weak<(Weak<IsolateState>, IValue)> {
        Arc::downgrade(&self.0)
    }

    #[inline(always)]
    fn drop_inner(self) {
        let _ = self.0;
    }
}

unsafe impl Send for ArcStackItem {}
unsafe impl Sync for ArcStackItem {}

/// A stack containing a bunch of a bunch of values
/// which can be dropped.
#[repr(transparent)]
#[derive(Debug)]
pub struct ArcStack {
    items: Vec<ArcStackItem>,
}

impl ArcStack {
    pub const fn new() -> Self {
        Self { items: vec![] }
    }

    pub fn push(&mut self, item: ArcStackItem) {
        self.items.push(item);
    }

    #[inline(always)]
    pub fn drop_all(&mut self) {
        self.items.drain(..).for_each(|item| item.drop_inner())
    }
}

impl Drop for ArcStack {
    fn drop(&mut self) {
        self.drop_all();
    }
}
