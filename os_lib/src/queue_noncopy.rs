#[allow(unsafe_op_in_unsafe_fn)]
use std::{
    alloc::{Layout, alloc, dealloc},
    io::Error,
    mem::{self, MaybeUninit},
    ptr,
    sync::atomic::{AtomicUsize, Ordering},
};

/// Lock-free single-producer single-consumer ring buffer for non-`Copy` types.
///
/// **Safety:** The implementation uses raw pointers and atomics; callers must respect the
/// single-reader / single-writer contract when using [`RWRoundQueue::split`].
pub struct RWRoundQueue<T> {
    buffer: *mut MaybeUninit<T>,
    capacity: usize,
    layout: Layout,

    // Atomic indices for lock-free access
    read_idx: AtomicUsize,
    write_idx: AtomicUsize,

    // Cached pointers for optimization
    start_ptr: *const T,
    end_ptr: *const T,
}

unsafe impl<T: Send> Send for RWRoundQueue<T> {}
unsafe impl<T: Send> Sync for RWRoundQueue<T> {}

impl<T> RWRoundQueue<T> {
    /// Allocates a new ring buffer with the given capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` — Number of slots; must be greater than zero and a power of two.
    ///
    /// # Returns
    ///
    /// `Ok(Self)` on success, or `Err(io::Error)` if capacity is invalid or allocation fails.
    pub fn new(capacity: usize) -> Result<Self, Error> {
        if capacity == 0 {
            return Err(Error::new(
                std::io::ErrorKind::InvalidInput,
                "Capacity must be greater than 0",
            ));
        }
        if !capacity.is_power_of_two() {
            return Err(Error::new(
                std::io::ErrorKind::InvalidInput,
                "Capacity must be power of 2",
            ));
        }

        let layout = Layout::array::<MaybeUninit<T>>(capacity).unwrap();

        unsafe {
            let buffer = alloc(layout) as *mut MaybeUninit<T>;
            if buffer.is_null() {
                return Err(Error::new(
                    std::io::ErrorKind::Other,
                    "Memory allocation failed",
                ));
            }

            // Initialize memory with MaybeUninit pattern
            for i in 0..capacity {
                ptr::write(buffer.add(i), mem::zeroed());
            }

            let start_ptr = buffer as *const T;
            let end_ptr = buffer.add(capacity) as *const T;

            Ok(Self {
                buffer,
                capacity,
                layout,
                read_idx: AtomicUsize::new(0),
                write_idx: AtomicUsize::new(0),
                start_ptr,
                end_ptr,
            })
        }
    }

    /// Splits this queue into a [`QueueReader`] and [`QueueWriter`] sharing the same buffer.
    ///
    /// # Arguments
    ///
    /// * `self` — Exclusive mutable access to the queue (must not be used directly after split).
    ///
    /// # Returns
    ///
    /// A `(reader, writer)` pair referencing this queue.
    ///
    /// # Safety
    ///
    /// Call **at most once** per queue instance; multiple splits violate aliasing rules.
    pub unsafe fn split(&mut self) -> (QueueReader<T>, QueueWriter<T>) {
        let reader = QueueReader {
            queue: self as *const Self,
            _phantom: std::marker::PhantomData,
        };

        let writer = QueueWriter {
            queue: self as *mut Self,
            _phantom: std::marker::PhantomData,
        };

        (reader, writer)
    }

    /// Returns the next slot index after `current`, wrapping at `capacity`.
    ///
    /// # Arguments
    ///
    /// * `current` — Index in `0..capacity`.
    ///
    /// # Returns
    ///
    /// Wrapped successor index.
    #[inline]
    fn next_index(&self, current: usize) -> usize {
        (current + 1) & (self.capacity - 1) // Fast modulo for power of 2
    }

    // ===== Pointer Management Layer =====

    /// Reserves the current read slot and returns a pointer to it (does not move out the value).
    ///
    /// # Arguments
    ///
    /// * `self` — The queue; only the reader side should call this.
    ///
    /// # Returns
    ///
    /// `Some(ptr)` to the slot to read, or `None` if the queue is empty.
    ///
    /// # Safety
    ///
    /// After reading through the pointer, the caller must call [`Self::commit_read`].
    pub unsafe fn acquire_read_ptr(&self) -> Option<*const MaybeUninit<T>> {
        let read_idx = self.read_idx.load(Ordering::Acquire);
        let write_idx = self.write_idx.load(Ordering::Acquire);

        // Empty check
        if read_idx == write_idx {
            return None;
        }

        // Return pointer to current read position
        let ptr = self.buffer.add(read_idx);
        Some(ptr as *const MaybeUninit<T>)
    }

    /// Advances the read index after a successful read via [`Self::acquire_read_ptr`].
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    ///
    /// # Returns
    ///
    /// `()`.
    ///
    /// # Safety
    ///
    /// Must follow a successful `acquire_read_ptr` for the same logical dequeue.
    pub unsafe fn commit_read(&self) {
        let read_idx = self.read_idx.load(Ordering::Acquire);
        let next_read = self.next_index(read_idx);
        self.read_idx.store(next_read, Ordering::Release);
    }

    /// Reserves the current write slot and returns a pointer plus overwrite flag.
    ///
    /// # Arguments
    ///
    /// * `self` — The queue; only the writer side should call this.
    ///
    /// # Returns
    ///
    /// Always `Some((ptr, was_full))` where `ptr` is the slot to write and `was_full` is `true`
    /// if the write would overwrite an unread value (caller should drop/replace the old `T` correctly).
    ///
    /// # Safety
    ///
    /// Caller must write a valid `T` (or handle overwrite), then call [`Self::commit_write`].
    pub unsafe fn acquire_write_ptr(&self) -> Option<(*mut MaybeUninit<T>, bool)> {
        let write_idx = self.write_idx.load(Ordering::Acquire);
        let read_idx = self.read_idx.load(Ordering::Acquire);
        let next_write = self.next_index(write_idx);

        // Check if full (would overtake read)
        let was_full = next_write == read_idx;

        let ptr = self.buffer.add(write_idx);
        Some((ptr, was_full))
    }

    /// Advances the write index (and read index if an overwrite occurred).
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    /// * `was_full` — The flag returned with the pointer from [`Self::acquire_write_ptr`].
    ///
    /// # Returns
    ///
    /// `()`.
    ///
    /// # Safety
    ///
    /// Must follow `acquire_write_ptr`; if `was_full`, the old slot must have been handled correctly.
    pub unsafe fn commit_write(&self, was_full: bool) {
        let write_idx = self.write_idx.load(Ordering::Acquire);
        let next_write = self.next_index(write_idx);

        if was_full {
            // Advance read pointer (we're overwriting)
            let read_idx = self.read_idx.load(Ordering::Acquire);
            let next_read = self.next_index(read_idx);
            self.read_idx.store(next_read, Ordering::Release);
        }

        self.write_idx.store(next_write, Ordering::Release);
    }

    // ===== Helper Methods =====

    /// Approximate number of elements between read and write indices.
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    ///
    /// # Returns
    ///
    /// Element count (may be stale if another thread mutates indices concurrently).
    pub fn len(&self) -> usize {
        let read_idx = self.read_idx.load(Ordering::Acquire);
        let write_idx = self.write_idx.load(Ordering::Acquire);

        if write_idx >= read_idx {
            write_idx - read_idx
        } else {
            self.capacity - read_idx + write_idx
        }
    }

    /// Returns `true` when read and write indices are equal (queue appears empty).
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    ///
    /// # Returns
    ///
    /// `true` if empty, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        let read_idx = self.read_idx.load(Ordering::Acquire);
        let write_idx = self.write_idx.load(Ordering::Acquire);
        read_idx == write_idx
    }

    /// Ring capacity configured at construction.
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    ///
    /// # Returns
    ///
    /// The capacity value.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    // ===== High-level Convenience APIs =====

    /// Pops one element if the queue is non-empty.
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    ///
    /// # Returns
    ///
    /// `Some(value)` or `None` if empty.
    ///
    /// # Safety
    ///
    /// Must be used from the reader side only, with proper happens-before ordering vs. the writer.
    pub unsafe fn try_read(&self) -> Option<T> {
        let ptr = self.acquire_read_ptr()?;
        let value = ptr::read(ptr).assume_init();
        self.commit_read();
        Some(value)
    }

    /// Enqueues `value`, overwriting the oldest element when the buffer is full.
    ///
    /// # Arguments
    ///
    /// * `self` — The queue.
    /// * `value` — Value to store in the next slot.
    ///
    /// # Returns
    ///
    /// `true` if an unread element was overwritten, `false` otherwise.
    ///
    /// # Safety
    ///
    /// Writer-side only. Overwriting may leak if the overwritten slot was initialized and not dropped
    /// (see FIXME comments in source).
    pub unsafe fn write_overwrite(&self, value: T) -> bool {
        let (ptr, was_full) = self.acquire_write_ptr().unwrap();

        // Check whether we need to drop old value
        // FIXME: We have no way to know whether T is init or not
        // We just simply always drop if overwriting. This is not accurate and unsafe
        // ptr::drop_in_place(ptr as *mut T);
        // FIXME: We don't drop the old value, which may cause memory leaks for non-Copy types

        ptr::write(ptr, MaybeUninit::new(value));
        self.commit_write(was_full);

        was_full
    }
}

impl<T> Drop for RWRoundQueue<T> {
    /// Drains readable elements then frees the backing allocation.
    ///
    /// # Arguments / returns
    ///
    /// Standard [`Drop`]: `&mut self` → `()`.
    fn drop(&mut self) {
        unsafe {
            // Drain remaining items
            while self.try_read().is_some() {}

            // Deallocate buffer
            dealloc(self.buffer as *mut u8, self.layout);
        }
    }
}

/// Reader handle for [`RWRoundQueue`]; may be moved to another thread (`Send`, not `Sync`).
pub struct QueueReader<T> {
    queue: *const RWRoundQueue<T>,
    _phantom: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for QueueReader<T> {}

impl<T> QueueReader<T> {
    /// Reads and removes the next element, or returns `None` if empty.
    ///
    /// # Arguments
    ///
    /// * `self` — Reader handle.
    ///
    /// # Returns
    ///
    /// `Some(T)` or `None`.
    pub fn read(&self) -> Option<T> {
        unsafe { (*self.queue).try_read() }
    }

    /// Drains up to `max` elements in order.
    ///
    /// # Arguments
    ///
    /// * `self` — Reader handle.
    /// * `max` — Upper bound on how many items to dequeue.
    ///
    /// # Returns
    ///
    /// A `Vec` of all values read (length ≤ `max`, may be empty).
    pub fn read_batch(&self, max: usize) -> Vec<T> {
        let mut result = Vec::with_capacity(max);

        for _ in 0..max {
            if let Some(item) = self.read() {
                result.push(item);
            } else {
                break;
            }
        }

        result
    }

    /// See [`RWRoundQueue::len`].
    ///
    /// # Arguments
    ///
    /// * `self` — Reader handle.
    ///
    /// # Returns
    ///
    /// Approximate queued length.
    pub fn len(&self) -> usize {
        unsafe { (*self.queue).len() }
    }

    /// See [`RWRoundQueue::is_empty`].
    ///
    /// # Arguments
    ///
    /// * `self` — Reader handle.
    ///
    /// # Returns
    ///
    /// `true` if no readable elements.
    pub fn is_empty(&self) -> bool {
        unsafe { (*self.queue).is_empty() }
    }
}

/// Writer handle for [`RWRoundQueue`]; may be moved to another thread (`Send`, not `Sync`).
pub struct QueueWriter<T> {
    queue: *mut RWRoundQueue<T>,
    _phantom: std::marker::PhantomData<T>,
}

unsafe impl<T: Send> Send for QueueWriter<T> {}

impl<T> QueueWriter<T> {
    /// Low-level: obtain the next write slot; pair with [`Self::commit`].
    ///
    /// # Arguments
    ///
    /// * `self` — Writer handle.
    ///
    /// # Returns
    ///
    /// `Some((ptr, was_full))` as in [`RWRoundQueue::acquire_write_ptr`], or `None` if unavailable.
    ///
    /// # Safety
    ///
    /// Caller must initialize the slot and then call `commit(was_full)`.
    pub unsafe fn acquire_ptr(&mut self) -> Option<(*mut MaybeUninit<T>, bool)> {
        (*self.queue).acquire_write_ptr()
    }

    /// Completes a write after [`Self::acquire_ptr`].
    ///
    /// # Arguments
    ///
    /// * `self` — Writer handle.
    /// * `was_full` — Flag from `acquire_ptr`.
    ///
    /// # Returns
    ///
    /// `()`.
    ///
    /// # Safety
    ///
    /// Must match a prior successful `acquire_ptr`.
    pub unsafe fn commit(&mut self, was_full: bool) {
        (*self.queue).commit_write(was_full)
    }

    /// Enqueues a value (overwriting when full); see [`RWRoundQueue::write_overwrite`].
    ///
    /// # Arguments
    ///
    /// * `self` — Writer handle.
    /// * `value` — Value to enqueue.
    ///
    /// # Returns
    ///
    /// `true` if an old element was overwritten.
    ///
    /// # Safety
    ///
    /// Writer side only; see queue safety notes for non-`Copy` `T`.
    pub unsafe fn write(&mut self, value: T) -> bool {
        (*self.queue).write_overwrite(value)
    }

    /// Returns configured capacity.
    ///
    /// # Arguments
    ///
    /// * `self` — Writer handle.
    ///
    /// # Returns
    ///
    /// Ring capacity.
    pub fn capacity(&self) -> usize {
        unsafe { (*self.queue).capacity() }
    }
}
