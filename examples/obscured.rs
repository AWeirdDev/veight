use core::{ops::Deref, ptr::NonNull};

#[repr(transparent)]
pub struct Obscured<T> {
    data_ptr: NonNull<T>,
}

unsafe impl<T> Send for Obscured<T> {}
unsafe impl<T> Sync for Obscured<T> {}

impl<T> Obscured<T> {
    #[inline(always)]
    #[must_use]
    pub fn new(x: Box<T>) -> Self {
        Self {
            data_ptr: unsafe { NonNull::new_unchecked(Box::into_raw(x)) },
        }
    }

    #[inline(always)]
    #[must_use]
    pub fn from_raw(ptr: *mut T) -> Option<Self> {
        NonNull::new(ptr).map(|data| Self { data_ptr: data })
    }

    #[inline(always)]
    #[must_use]
    pub unsafe fn from_raw_unchecked(ptr: *mut T) -> Self {
        Self {
            data_ptr: unsafe { NonNull::new_unchecked(ptr) },
        }
    }

    #[inline(always)]
    #[must_use]
    pub fn into_raw(this: Obscured<T>) -> *mut T {
        this.data_ptr.cast::<T>().as_ptr()
    }

    #[inline(always)]
    #[must_use]
    pub unsafe fn take_unchecked(self) -> Box<T> {
        unsafe { Box::from_raw(self.data_ptr.cast::<T>().as_ptr()) }
    }

    #[inline(always)]
    #[must_use]
    /// Gets a reference.
    pub const fn as_ref(&self) -> &T {
        unsafe { &*self.data_ptr.cast::<T>().as_ptr() }
    }

    #[inline(always)]
    #[must_use]
    /// Gets the mutable reference.
    ///
    /// # Safety
    /// **This does not gurantee any safety at all.**
    /// If the data is mutable, use [`MutObscuredEx`] instead.
    pub const unsafe fn as_mut_unchecked(&self) -> &mut T {
        unsafe { &mut *self.data_ptr.cast::<T>().as_ptr() }
    }
}

impl<T> Deref for Obscured<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl<T> Drop for Obscured<T> {
    fn drop(&mut self) {
        let _ = unsafe { Box::from_raw(self.data_ptr.cast::<T>().as_ptr()) };
    }
}

fn main() {}
