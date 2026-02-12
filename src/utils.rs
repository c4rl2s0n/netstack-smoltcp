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
    pub fn get_buffer(&mut self, len: usize) -> BytesMut {
        // If we don't have enough contiguous space, allocate a new "chunk"
        if self.pool.capacity() < len {
            // Allocate a large chunk to amortize allocation costs
            // If the requested buffer is larger than the whole capacity, allocate that amount of bytes
            // self.pool = BytesMut::with_capacity(std::cmp::max(self.capacity, len));
            let required = std::cmp::max(self.capacity, len);
            self.pool.reserve(required);
        }
        self.pool.split_to(len)
    }
}
