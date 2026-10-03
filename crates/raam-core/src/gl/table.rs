//! A table of WebGL objects named by GL-style integers (the WebGL1
//! linkage's, kept apart so its tests run natively). 0 stays "none", as in
//! GL, and a deleted name is handed out again, as GL's are: the slideshow
//! makes and deletes render targets every slide, so a table that only
//! grew would grow for as long as the page stays open.

use super::GlUint;

pub(super) struct Table<T>(Vec<Option<T>>);

impl<T: Clone> Table<T> {
    pub(super) const fn new() -> Self {
        Self(Vec::new())
    }

    pub(super) fn add(&mut self, v: T) -> GlUint {
        if self.0.is_empty() {
            self.0.push(None); // 0 is "none"
        }
        let id = match self.0.iter().skip(1).position(Option::is_none) {
            Some(free) => {
                self.0[free + 1] = Some(v);
                free + 1
            }
            None => {
                self.0.push(Some(v));
                self.0.len() - 1
            }
        };
        GlUint::try_from(id).expect("live GL objects fit a GL name")
    }

    pub(super) fn get(&self, id: GlUint) -> Option<T> {
        self.0.get(id as usize).cloned().flatten()
    }

    pub(super) fn remove(&mut self, id: GlUint) -> Option<T> {
        self.0.get_mut(id as usize).and_then(Option::take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_start_at_one() {
        let mut t = Table::new();
        assert_eq!((t.add(10), t.add(11)), (1, 2));
        assert_eq!(t.get(0), None);
        assert_eq!(t.get(2), Some(11));
    }

    #[test]
    fn a_deleted_name_is_reused_so_the_table_stays_bounded() {
        let mut t = Table::new();
        let keep = t.add(1);
        for slide in 0..1000 {
            let target = t.add(slide);
            assert_eq!(t.remove(target), Some(slide));
        }
        assert_eq!(t.0.len(), 3, "none, the kept object and one reused slot");
        assert_eq!(t.get(keep), Some(1));
    }

    #[test]
    fn a_removed_name_reads_as_none() {
        let mut t = Table::new();
        let id = t.add(7);
        assert_eq!(t.remove(id), Some(7));
        assert_eq!((t.get(id), t.remove(id)), (None, None));
    }
}
