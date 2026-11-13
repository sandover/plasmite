//! Explicit independent ownership for a selected child, sharing its snapshot.
use super::{MessageView, ValueRef};

/// A selected value that keeps its complete immutable snapshot alive.
#[derive(Clone)]
pub struct OwnedValue {
    owner: MessageView,
    offset: usize,
}
impl ValueRef<'_> {
    /// Retain this child independently, sharing encoded bytes without copying.
    pub fn into_owned(self) -> OwnedValue {
        OwnedValue {
            owner: self.owner.clone(),
            offset: self.offset,
        }
    }
}
impl OwnedValue {
    /// Borrow this value for traversal or typed decoding.
    pub fn as_ref(&self) -> ValueRef<'_> {
        ValueRef {
            owner: &self.owner,
            offset: self.offset,
        }
    }
}
impl serde::Serialize for OwnedValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.as_ref().serialize(serializer)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_child_outlives_parent_without_copying_snapshot() {
        let parent = MessageView::from_json(br#"{"nested":{"n":7},"other":[1,2,3]}"#, &[]).unwrap();
        let pointer = parent.bytes().as_ptr();
        let child = parent.data().get("nested").unwrap().unwrap().into_owned();
        assert_eq!(child.owner.bytes().as_ptr(), pointer);
        drop(parent);
        assert_eq!(
            child
                .as_ref()
                .get("n")
                .unwrap()
                .unwrap()
                .decode::<i64>()
                .unwrap(),
            7
        );
        let output = MessageView::encode(&child, &[]).unwrap();
        assert_eq!(output.data().get("n").unwrap().unwrap().integer(), Some(7));
    }
}
