//! Lock access that treats a poisoned lock as the bug it is: an earlier holder panicked.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

const POISONED: &str = "lock poisoned: a holder panicked";

/// Locks a [`Mutex`], panicking if an earlier holder panicked.
pub trait MutexExt<T: ?Sized> {
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T: ?Sized> MutexExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().expect(POISONED)
    }
}

/// Locks an [`RwLock`], panicking if an earlier holder panicked.
pub trait RwLockExt<T: ?Sized> {
    fn read_locked(&self) -> RwLockReadGuard<'_, T>;
    fn write_locked(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T: ?Sized> RwLockExt<T> for RwLock<T> {
    fn read_locked(&self) -> RwLockReadGuard<'_, T> {
        self.read().expect(POISONED)
    }

    fn write_locked(&self) -> RwLockWriteGuard<'_, T> {
        self.write().expect(POISONED)
    }
}
