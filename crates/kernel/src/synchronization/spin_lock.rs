//! A mutual exclusion lock that spins until it is free.

use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

/// Protects a value so only one core touches it at a time.
pub struct SpinLock<Value> {
    is_locked: AtomicBool,
    value: UnsafeCell<Value>,
}

// SAFETY: The lock hands out access to `value` to one holder at a time, so sharing the lock
// between cores is sound whenever the value itself may move between cores.
unsafe impl<Value: Send> Sync for SpinLock<Value> {}

impl<Value> SpinLock<Value> {
    /// Creates an unlocked lock around `value`.
    pub const fn new(value: Value) -> Self {
        Self {
            is_locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// Spins until the lock is free, then holds it until the guard is dropped.
    pub fn lock(&self) -> SpinLockGuard<'_, Value> {
        while self
            .is_locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
        SpinLockGuard { lock: self }
    }
}

/// Proof that the lock is held. Releases it on drop.
pub struct SpinLockGuard<'lock, Value> {
    lock: &'lock SpinLock<Value>,
}

impl<Value> Deref for SpinLockGuard<'_, Value> {
    type Target = Value;

    fn deref(&self) -> &Value {
        // SAFETY: The guard exists only while the lock is held, so no one else has access.
        unsafe { &*self.lock.value.get() }
    }
}

impl<Value> DerefMut for SpinLockGuard<'_, Value> {
    fn deref_mut(&mut self) -> &mut Value {
        // SAFETY: The guard exists only while the lock is held, so access is exclusive.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<Value> Drop for SpinLockGuard<'_, Value> {
    fn drop(&mut self) {
        self.lock.is_locked.store(false, Ordering::Release);
    }
}