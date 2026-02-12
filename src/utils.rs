use tokio_util::bytes::BytesMut;

pub struct BufferPool {
    pool: BytesMut,
    capacity: usize,
}

impl BufferPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            pool: BytesMut::with_capacity(capacity),
            capacity,
        }
    }
    /// Acquire a buffer of the requested length from the BufferPool
    /// The buffer is dirty in the sense, that it might contain random memory that was stored before
    /// 
    /// WARNING: use this only, if you are sure the buffer will be overridden before reading from it to prevent potentially leaking information!
    pub fn get_dirty_buffer(&mut self, len: usize) -> BytesMut {
        // If we don't have enough contiguous space, allocate a new "chunk"
        if self.pool.capacity() < len {
            // Allocate a large chunk to amortize allocation costs
            // If the requested buffer is larger than the whole capacity, allocate that amount of bytes
            let required = std::cmp::max(self.capacity, len);
             // acquire a new chunk of memory, to avoid growing the pool indefinitely and allow for early free on small chunks
            self.pool = BytesMut::with_capacity(required);
        }
        // Assert that the buffer has the capacity to set the length! 
        // The only "unsafe" thing left is, that the buffer may contain random old data.
        // NOTE: we make sure that enough space is actually reserved before, so we set_len to get the buffer, containing potentially dirty memory
        assert_eq!(self.pool.capacity(), len, "The acquired buffer must have the capacity for the required length!");
        unsafe { self.pool.set_len(len) };
        self.pool.split()
    }
}
