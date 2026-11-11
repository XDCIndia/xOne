use std::collections::VecDeque;

/// What a full queue does with a new item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overflow {
    /// Evict the oldest item, keep the new one (stale votes, head blocks).
    DropOldest,
    /// Discard the new item (tx gossip, log lines, RPC admission).
    Reject,
    /// Discard the new item and tell the caller to drop the peer.
    DisconnectPeer,
    /// Keep nothing new; give the item back so the producer must wait.
    /// Only for internal producers, never for a shared peer socket.
    Backpressure,
}

/// Result of [`BoundedQueue::push`]. Items not stored are handed back.
#[derive(Debug, PartialEq, Eq)]
pub enum Push<T> {
    Accepted,
    /// The new item is stored; this older item was evicted.
    Evicted(T),
    Rejected(T),
    Disconnect(T),
    Full(T),
}

/// Fixed-capacity FIFO with an explicit overflow policy and a drop counter.
///
/// ponytail: single owner, no locking. The thread-safe wrapper (Mutex plus
/// Condvar or a channel) belongs to node wiring; add it there when a queue
/// crosses threads.
#[derive(Debug)]
pub struct BoundedQueue<T> {
    items: VecDeque<T>,
    cap: usize,
    policy: Overflow,
    dropped: u64,
}

impl<T> BoundedQueue<T> {
    /// `cap` is clamped to at least 1: a zero bound would drop everything.
    pub fn new(cap: usize, policy: Overflow) -> Self {
        let cap = cap.max(1);
        Self {
            items: VecDeque::with_capacity(cap.min(1024)),
            cap,
            policy,
            dropped: 0,
        }
    }

    pub fn push(&mut self, item: T) -> Push<T> {
        if self.items.len() < self.cap {
            self.items.push_back(item);
            return Push::Accepted;
        }
        self.dropped += 1;
        match self.policy {
            Overflow::DropOldest => {
                let old = self.items.pop_front().expect("cap >= 1 and full");
                self.items.push_back(item);
                Push::Evicted(old)
            }
            Overflow::Reject => Push::Rejected(item),
            Overflow::DisconnectPeer => Push::Disconnect(item),
            Overflow::Backpressure => Push::Full(item),
        }
    }

    pub fn pop(&mut self) -> Option<T> {
        self.items.pop_front()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Overflow events since creation (for the per-queue counter).
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(policy: Overflow) -> BoundedQueue<u32> {
        let mut q = BoundedQueue::new(2, policy);
        assert_eq!(q.push(1), Push::Accepted);
        assert_eq!(q.push(2), Push::Accepted);
        q
    }

    #[test]
    fn drop_oldest_keeps_newest() {
        let mut q = filled(Overflow::DropOldest);
        assert_eq!(q.push(3), Push::Evicted(1));
        assert_eq!((q.pop(), q.pop(), q.pop()), (Some(2), Some(3), None));
        assert_eq!(q.dropped(), 1);
    }

    #[test]
    fn reject_keeps_oldest() {
        let mut q = filled(Overflow::Reject);
        assert_eq!(q.push(3), Push::Rejected(3));
        assert_eq!((q.pop(), q.pop()), (Some(1), Some(2)));
    }

    #[test]
    fn disconnect_and_backpressure_hand_item_back() {
        let mut q = filled(Overflow::DisconnectPeer);
        assert_eq!(q.push(3), Push::Disconnect(3));
        assert_eq!(q.len(), 2);
        let mut q = filled(Overflow::Backpressure);
        assert_eq!(q.push(3), Push::Full(3));
        q.pop();
        assert_eq!(q.push(3), Push::Accepted);
    }

    #[test]
    fn never_exceeds_bound_under_flood() {
        for p in [
            Overflow::DropOldest,
            Overflow::Reject,
            Overflow::DisconnectPeer,
            Overflow::Backpressure,
        ] {
            let mut q = BoundedQueue::new(8, p);
            for i in 0..10_000 {
                q.push(i);
                assert!(q.len() <= 8);
            }
            assert_eq!(q.dropped(), 10_000 - 8);
        }
    }

    #[test]
    fn zero_cap_is_clamped() {
        let mut q = BoundedQueue::new(0, Overflow::Reject);
        assert_eq!(q.push(1), Push::Accepted);
        assert_eq!(q.push(2), Push::Rejected(2));
    }
}
