//! Implements the [`StdRawRwLock`] helper type which wraps a
//! [`std::syn::RwLock`] so that it can be used with `lock_api`

use {
    core::ops::Deref,
    std::{
        cell::UnsafeCell,
        fmt,
        mem::MaybeUninit,
        pin::Pin,
        sync::{Once, RwLock, RwLockWriteGuard, TryLockError},
    },
};

use lock_api::RawRwLock;

pub struct StdRawRwLock {
    lock: LazyPinBox<RwLock<()>>,
    write: UnsafeCell<Option<RwLockWriteGuard<'static, ()>>>,
}

#[allow(unsafe_code)]
unsafe impl Send for StdRawRwLock {}

#[allow(unsafe_code)]
unsafe impl Sync for StdRawRwLock {}

impl fmt::Debug for StdRawRwLock {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        fmt.debug_struct("StdRawRwLock").finish()
    }
}

impl Drop for StdRawRwLock {
    fn drop(&mut self) {
        // Drop any outstanding write guard before the lock
        std::mem::drop(self.write.get_mut().take());
    }
}

struct LazyPinBox<T: Default> {
    init: Once,
    data: UnsafeCell<MaybeUninit<Pin<Box<T>>>>,
}

impl<T: Default> LazyPinBox<T> {
    const fn new() -> Self {
        Self {
            init: Once::new(),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

impl<T: Default> Drop for LazyPinBox<T> {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        if self.init.is_completed() {
            unsafe { (*self.data.get()).assume_init_drop() };
        }
    }
}

impl<T: Default> Deref for LazyPinBox<T> {
    type Target = T;

    #[allow(unsafe_code)]
    fn deref(&self) -> &Self::Target {
        self.init.call_once(|| unsafe {
            (*self.data.get()).write(Box::pin(T::default()));
        });

        unsafe { (*self.data.get()).assume_init_ref() }
    }
}

#[allow(unsafe_code)]
unsafe impl RawRwLock for StdRawRwLock {
    #[allow(clippy::declare_interior_mutable_const)]
    const INIT: Self = StdRawRwLock {
        lock: LazyPinBox::new(),
        write: UnsafeCell::new(None),
    };

    type GuardMarker = lock_api::GuardNoSend;

    fn lock_shared(&self) {
        let guard = self
            .lock
            .read()
            .expect("interner lock should not be poisoned");

        // The read guard can be reconstructed later from another read
        std::mem::forget(guard);
    }

    fn try_lock_shared(&self) -> bool {
        let guard = match self.lock.try_read() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return false,
            r @ Err(TryLockError::Poisoned(_)) => r.expect("interner lock should not be poisoned"),
        };

        // The read guard can be reconstructed later from another read
        std::mem::forget(guard);

        true
    }

    unsafe fn unlock_shared(&self) {
        // Since this method may only be called if a shared lock is held,
        // this if branch should always be taken
        if let Ok(new_guard) = self.lock.try_read() {
            // Safety:
            // - unlock_shared may only be called if a shared lock is held
            //   in the current context
            // - thus an old guard for that shared lock must exist
            let old_guard = std::ptr::read(&new_guard);

            std::mem::drop(old_guard);
            std::mem::drop(new_guard);
        }
    }

    fn lock_exclusive<'a>(&'a self) {
        let guard: RwLockWriteGuard<'a, ()> = self
            .lock
            .write()
            .expect("interner lock should not be poisoned");
        // Safety: lifetime erasure to store a never-exposed reference to self
        let guard: RwLockWriteGuard<'static, ()> = unsafe { std::mem::transmute(guard) };

        // Safety:
        // - the RwLock is pinned inside the LazyPinBox,
        //   so any references to it remain valid
        // - the interior mutability write occurs while the unique
        //   write lock guard is held
        unsafe { *self.write.get() = Some(guard) };
    }

    fn try_lock_exclusive<'a>(&'a self) -> bool {
        let guard: RwLockWriteGuard<'a, ()> = match self.lock.try_write() {
            Ok(guard) => guard,
            Err(TryLockError::WouldBlock) => return false,
            r @ Err(TryLockError::Poisoned(_)) => r.expect("interner lock should not be poisoned"),
        };
        // Safety: lifetime erasure to store a never-exposed reference to self
        let guard: RwLockWriteGuard<'static, ()> = unsafe { std::mem::transmute(guard) };

        // Safety:
        // - the RwLock is pinned inside the LazyPinBox,
        //   so any references to it remain valid
        // - the interior mutability write occurs while the unique
        //   write lock guard is held
        unsafe { *self.write.get() = Some(guard) };

        true
    }

    unsafe fn unlock_exclusive<'a>(&'a self) {
        // Safety:
        // - unlock_exclusive may only be called if an exclusive lock is held
        //   in the current context -> the if branch is taken
        // - the interior mutability write occurs while the unique
        //   write lock guard is held
        if let Some(guard) = unsafe { (*self.write.get()).take() } {
            // Safety: lifetime un-erasure for the self-referential guard
            let guard: RwLockWriteGuard<'a, ()> = unsafe { std::mem::transmute(guard) };

            std::mem::drop(guard);
        }
    }
}
