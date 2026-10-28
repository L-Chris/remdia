//! Reservations account once per returned allocation, regardless of references.
use anyhow::{Result, ensure};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Clone, Debug)]
pub struct Budget(Arc<Inner>);
#[derive(Debug)]
struct Inner {
    limit: usize,
    used: AtomicUsize,
    peak: AtomicUsize,
}
#[derive(Debug)]
pub struct Reservation {
    budget: Budget,
    bytes: usize,
}
impl Budget {
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Inner {
            limit,
            used: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
        }))
    }
    pub fn reserve(&self, bytes: usize) -> Result<Reservation> {
        let mut used = self.0.used.load(Ordering::Acquire);
        loop {
            ensure!(
                bytes <= self.0.limit.saturating_sub(used),
                "managed buffer budget exhausted; release held frames before reading"
            );
            match self.0.used.compare_exchange_weak(
                used,
                used + bytes,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.0.peak.fetch_max(used + bytes, Ordering::Relaxed);
                    break;
                }
                Err(n) => used = n,
            }
        }
        Ok(Reservation {
            budget: self.clone(),
            bytes,
        })
    }
    pub fn limit(&self) -> usize {
        self.0.limit
    }
    pub fn used(&self) -> usize {
        self.0.used.load(Ordering::Acquire)
    }
    pub fn peak(&self) -> usize {
        self.0.peak.load(Ordering::Relaxed)
    }
}
impl Reservation {
    pub fn shares_budget_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.budget.0, &other.budget.0)
    }
    pub fn shrink(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.budget
            .0
            .used
            .fetch_sub(self.bytes - bytes, Ordering::AcqRel);
        self.bytes = bytes;
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_frames_hold_their_reservation() {
        let b = Budget::new(8);
        let r = Arc::new(b.reserve(8).unwrap());
        let other = r.clone();
        drop(r);
        assert!(b.reserve(1).is_err());
        drop(other);
        assert_eq!(b.used(), 0);
        assert_eq!(b.peak(), 8);
    }
}
