mod resource_state;
mod worker;
mod allocator;
mod recorder;

use std::collections::{VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use ash::vk;

use crate::prelude::*;
use crate::device::device::VkQueue;
use crate::vk::objects::allocator::Allocator;
use crate::vk::objects::buffer::Buffer;

use worker::*;
use crate::objects::id::{BufferId, ImageId, ObjectId};
use crate::objects::sync::{SemaphoreOp, SemaphoreOps};
use crate::vk::objects::image::Image;

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum AcquireError {
    AlreadyAvailable,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum ReleaseError {
    NotAvailable,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct SyncId(u64);

impl SyncId {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    pub fn get_raw(&self) -> u64 {
        self.0
    }
}

pub struct Transfer {
    share: Arc<Share>,
    queue_family: u32,
    worker: Option<JoinHandle<()>>,
}

impl Transfer {
    pub fn new(device: Arc<DeviceContext>, alloc: Arc<Allocator>, queue: VkQueue) -> Self {
        let queue_family = queue.get_queue_family_index();
        let share = Arc::new(Share::new(device, alloc));

        let share2 = share.clone();
        let worker = std::thread::spawn(move || {
            run_worker(share2, queue)
        });

        Self {
            share,
            queue_family,
            worker: Some(worker)
        }
    }

    /// Returns the queue family index of the queue that is used for transfer operations.
    pub fn get_queue_family(&self) -> u32 {
        self.queue_family
    }

    /// Generates a buffer acquire operation for some buffer.
    ///
    /// This does **not** make the buffer available. It just collects information about the buffer
    /// and generates a potential memory barrier so that the calling code may submit it.
    ///
    /// - If `usage` is [`None`] no memory barrier will be generated.
    /// - If `usage` is [`Some`] the contents should be the source stage mask, source access mask
    /// and the source queue family index needed for a potential barrier. This function will
    /// determine if a memory barrier is necessary and if so generate one.
    pub fn prepare_buffer_acquire(&self, buffer: Buffer, usage: Option<(vk::PipelineStageFlags2, vk::AccessFlags2, u32)>) -> BufferAcquireOp {
        if let Some((src_stage_mask, src_access_mask, src_queue_family)) = usage {
            let queue_info =
                if src_queue_family == self.queue_family {
                    None
                } else {
                    Some((src_queue_family, self.queue_family))
                };

            BufferAcquireOp {
                buffer,
                offset: 0,
                size: vk::WHOLE_SIZE,
                src_info: Some((src_stage_mask, src_access_mask)),
                queue_info,
            }
        } else {
            BufferAcquireOp {
                buffer,
                offset: 0,
                size: vk::WHOLE_SIZE,
                src_info: None,
                queue_info: None
            }
        }
    }

    /// Makes a buffer available for transfer operations.
    ///
    /// The `op` can be generated by a call to [`prepare_buffer_acquire`]. If that call has
    /// generated a memory barrier the calling code **must** submit that barrier before calling this
    /// function.
    ///
    /// A list of wait semaphores can be provided through `semaphores`. All provided semaphores must
    /// have been submitted before this function is called.
    pub fn acquire_buffer(&self, op: BufferAcquireOp, semaphores: SemaphoreOps) -> Result<(), AcquireError> {
        self.share.push_task(Task::BufferAcquire(op, semaphores));
        Ok(())
    }

    /// Generates a buffer release operation for some buffer.
    ///
    /// This does **not** release the buffer. It just collects information about the buffer and
    /// generates a potential memory barrier.
    ///
    /// - If `usage` is [`None`] no memory barrier will be generated.
    /// - If `usage` is [`Some`] the contents should be the destination stage mask, destination
    /// access mask and the destination queue family index needed for a potential barrier. This
    /// function will determine if a memory barrier is necessary and if so generate one.
    pub fn prepare_buffer_release(&self, buffer: Buffer, usage: Option<(vk::PipelineStageFlags2, vk::AccessFlags2, u32)>) -> BufferReleaseOp {
        if let Some((dst_stage_mask, dst_access_mask, dst_queue_family)) = usage {
            let queue_info =
                if dst_queue_family == self.queue_family {
                    None
                } else {
                    Some((self.queue_family, dst_queue_family))
                };

            BufferReleaseOp {
                buffer,
                offset: 0,
                size: vk::WHOLE_SIZE,
                dst_info: Some((dst_stage_mask, dst_access_mask)),
                queue_info,
            }
        } else {
            BufferReleaseOp {
                buffer,
                offset: 0,
                size: vk::WHOLE_SIZE,
                dst_info: None,
                queue_info: None
            }
        }
    }

    /// Revokes availability of a buffer from transfer operations.
    ///
    /// The `op` can be generated by a call to [`prepare_buffer_release`]. If that call has
    /// generated a memory barrier the calling code **must** submit that barrier before using the
    /// buffer.
    ///
    /// Submission may happen asynchronously. As such the calling code must to call
    /// [`wait_for_submit`] with the returned id before submitting command buffers with a potential
    /// acquire barrier.
    ///
    /// A wait semaphore for future submissions can be generated by calling
    /// [`generate_wait_semaphore`] with the returned id.
    pub fn release_buffer(&self, op: BufferReleaseOp) -> Result<SyncId, ReleaseError> {
        let id = self.share.push_buffer_release_task(op);
        Ok(SyncId::from_raw(id))
    }

    /// Generates a image acquire operation for some image.
    ///
    /// This does **not** make the image available. It just collects information about the image
    /// and generates a potential memory barrier so that the calling code may submit it.
    ///
    /// - If `usage` is [`None`] no memory barrier will be generated. Initial layout will be assumed
    /// to be [`vk::ImageLayout::UNDEFINED`].
    /// - If `usage` is [`Some`] the contents should be the source stage mask, source access mask,
    /// the source queue family index and the source image layout. This function will determine if a
    /// memory barrier is necessary and if so generate one.
    pub fn prepare_image_acquire(&self, image: Image, aspect_mask: vk::ImageAspectFlags, usage: Option<(vk::PipelineStageFlags2, vk::AccessFlags2, u32, vk::ImageLayout)>) -> ImageAvailabilityOp {
        let (barrier, local_layout) = if let Some((stage, access, family, layout)) = usage {
            // If the layout is undefined there is no need for a barrier
            if layout != vk::ImageLayout::UNDEFINED && (family != self.queue_family || layout != vk::ImageLayout::GENERAL) {
                (Some(vk::ImageMemoryBarrier2::builder()
                    .src_stage_mask(stage)
                    .src_access_mask(access)
                    .dst_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                    .dst_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)
                    .old_layout(layout)
                    .new_layout(vk::ImageLayout::GENERAL)
                    .src_queue_family_index(family)
                    .dst_queue_family_index(self.queue_family)
                    .image(image.get_handle())
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: 0,
                        layer_count: vk::REMAINING_ARRAY_LAYERS
                    })
                    .build()
                ), vk::ImageLayout::GENERAL)
            } else {
                (None, layout)
            }
        } else {
            (None, vk::ImageLayout::UNDEFINED)
        };

        ImageAvailabilityOp {
            image,
            aspect_mask,
            local_layout,
            barrier
        }
    }

    /// Makes a image available for transfer operations.
    ///
    /// The `op` can be generated by a call to [`prepare_image_acquire`]. If that call has
    /// generated a memory barrier the calling code **must** submit that barrier before calling this
    /// function.
    ///
    /// A list of wait semaphores can be provided through `semaphores`. All provided semaphores must
    /// have been submitted before this function is called.
    pub fn make_image_available(&self, op: ImageAvailabilityOp, semaphores: SemaphoreOps) -> Result<(), AcquireError> {
        self.share.push_task(Task::ImageAcquire(op, semaphores));
        Ok(())
    }

    /// Generates a image release operation for some image.
    ///
    /// This does **not** release the image. It just collects information about the image and
    /// generates a potential memory barrier.
    ///
    /// - If `usage` is [`None`] no memory barrier will be generated.
    /// - If `usage` is [`Some`] the contents should be the destination stage mask, destination
    /// access maks, destination queue family and destination image layout. This function will
    /// determine if a memory barrier is necessary and if so generate one.
    pub fn prepare_image_release(&self, image: Image, aspect_mask: vk::ImageAspectFlags, usage: Option<(vk::PipelineStageFlags2, vk::AccessFlags2, u32, vk::ImageLayout)>) -> ImageAvailabilityOp {
        let (barrier, local_layout) = if let Some((stage, access, family, layout)) = usage {
            // If the layout is undefined there is no need for a barrier
            if layout != vk::ImageLayout::UNDEFINED && (family != self.queue_family) {
                (Some(vk::ImageMemoryBarrier2::builder()
                    .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                    .src_access_mask(vk::AccessFlags2::TRANSFER_READ | vk::AccessFlags2::TRANSFER_WRITE)
                    .dst_stage_mask(stage)
                    .dst_access_mask(access)
                    .old_layout(vk::ImageLayout::GENERAL)
                    .new_layout(layout)
                    .src_queue_family_index(self.queue_family)
                    .dst_queue_family_index(family)
                    .image(image.get_handle())
                    .subresource_range(vk::ImageSubresourceRange {
                        aspect_mask,
                        base_mip_level: 0,
                        level_count: vk::REMAINING_MIP_LEVELS,
                        base_array_layer: 0,
                        layer_count: vk::REMAINING_ARRAY_LAYERS
                    })
                    .build()
                ), vk::ImageLayout::GENERAL)
            } else {
                (None, layout)
            }
        } else {
            (None, vk::ImageLayout::UNDEFINED)
        };

        ImageAvailabilityOp {
            image,
            aspect_mask,
            local_layout,
            barrier,
        }
    }

    /// Revokes availability of a image from transfer operations.
    ///
    /// The `op` can be generated by a call to [`prepare_image_release`]. If that call has
    /// generated a memory barrier the calling code **must** submit that barrier before using the
    /// image.
    ///
    /// Submission may happen asynchronously. As such the calling code must to call
    /// [`wait_for_submit`] with the returned id before submitting command buffers with a potential
    /// acquire barrier.
    ///
    /// A wait semaphore for future submissions can be generated by calling
    /// [`generate_wait_semaphore`] with the returned id.
    pub fn release_image(&self, op: ImageAvailabilityOp) -> Result<SyncId, ReleaseError> {
        let id = self.share.push_image_release_task(op);
        Ok(SyncId::from_raw(id))
    }

    /// Returns some staging memory which can be used to upload to or download data from the device.
    ///
    /// The returned memory is at least as large as `capacity` but may be larger.
    pub fn request_staging_memory(&self, capacity: usize) -> StagingMemory {
        let (id, alloc) = self.share.allocate_staging(capacity as vk::DeviceSize);

        StagingMemory {
            transfer: &self,
            memory: unsafe {
                std::slice::from_raw_parts_mut(alloc.get_memory().as_ptr(), alloc.get_size() as usize)
            },
            memory_id: id,
            buffer_offset: alloc.get_offset()
        }
    }

    pub fn flush(&self, id: SyncId) {
        self.share.push_task(Task::Flush(id.get_raw()));
    }

    pub fn wait_for_submit(&self, id: SyncId) {
        self.share.wait_for_submit(id.get_raw());
    }

    pub fn wait_for_complete(&self, id: SyncId) {
        self.share.wait_for_complete(id.get_raw());
    }

    pub fn generate_wait_semaphore(&self, id: SyncId) -> SemaphoreOp {
        self.share.get_sync_wait_op(id.get_raw())
    }
}

impl Drop for Transfer {
    fn drop(&mut self) {
        self.share.terminate();
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Err(_) => {
                    log::error!("Transfer channel worker panicked!");
                },
                _ => {}
            }
        } else {
            log::error!("Transfer channel worker join handle has been taken before drop!");
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BufferAcquireOp {
    buffer: Buffer,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    src_info: Option<(vk::PipelineStageFlags2, vk::AccessFlags2)>,
    queue_info: Option<(u32, u32)>,
}

impl BufferAcquireOp {
    /// Returns a barrier which needs to be submitted by the user of the transfer engine before
    /// calling [`Transfer::acquire_buffer`]. If [`None`] is returned no barrier needs to be
    /// submitted.
    pub fn make_barrier(&self) -> Option<vk::BufferMemoryBarrier2> {
        // We only generate a user barrier if a queue family transfer is necessary
        self.queue_info.as_ref().map(|(src_queue_family, dst_queue_family)| {
            let (src_stage_mask, src_access_mask) = self.src_info.as_ref().unwrap();

            vk::BufferMemoryBarrier2::builder()
                .buffer(self.buffer.get_handle())
                .offset(self.offset)
                .size(self.size)
                .src_stage_mask(*src_stage_mask)
                .src_access_mask(*src_access_mask)
                .src_queue_family_index(*src_queue_family)
                .dst_queue_family_index(*dst_queue_family)
                .build()
        })
    }

    /// Returns the buffer used in this op
    pub fn get_buffer(&self) -> Buffer {
        self.buffer
    }

    /// Returns a barrier which needs to be submitted by the transfer engine before using the buffer.
    fn make_transfer_barrier(&self, dst_stage_mask: vk::PipelineStageFlags2, dst_access_mask: vk::AccessFlags2) -> Option<vk::BufferMemoryBarrier2> {
        self.src_info.as_ref().map(|(src_stage_mask, src_access_mask)| {
            let mut barrier = vk::BufferMemoryBarrier2::builder()
                .buffer(self.buffer.get_handle())
                .offset(self.offset)
                .size(self.size)
                .dst_stage_mask(dst_stage_mask)
                .dst_access_mask(dst_access_mask);

            if let Some((src_queue_family, dst_queue_family)) = &self.queue_info {
                barrier = barrier
                    .src_queue_family_index(*src_queue_family)
                    .dst_queue_family_index(*dst_queue_family);
            } else {
                barrier = barrier
                    .src_stage_mask(*src_stage_mask)
                    .src_access_mask(*src_access_mask);
            }

            barrier.build()
        })
    }
}

#[derive(Copy, Clone, Debug)]
pub struct BufferReleaseOp {
    buffer: Buffer,
    offset: vk::DeviceSize,
    size: vk::DeviceSize,
    dst_info: Option<(vk::PipelineStageFlags2, vk::AccessFlags2)>,
    queue_info: Option<(u32, u32)>,
}

impl BufferReleaseOp {
    /// Returns a barrier which needs to be submitted by the user of the transfer engine after
    /// calling [`Transfer::release_buffer`]. If [`None`] is returned no barrier needs to be
    /// submitted.
    ///
    /// Note that vulkan requires a potential queue family acquire barrier to be submitted after
    /// its corresponding release barrier. Since operations in the transfer engine are submitted
    /// asynchronously the user may need to call [`Transfer::wait_for_submit`] before submitting
    /// this barrier.
    pub fn make_barrier(&self) -> Option<vk::BufferMemoryBarrier2> {
        // We only generate a user barrier if a queue family transfer is necessary
        self.queue_info.as_ref().map(|(src_queue_family, dst_queue_family)| {
            let (dst_stage_mask, dst_access_mask) = self.dst_info.as_ref().unwrap();

            vk::BufferMemoryBarrier2::builder()
                .buffer(self.buffer.get_handle())
                .offset(self.offset)
                .size(self.size)
                .dst_stage_mask(*dst_stage_mask)
                .dst_access_mask(*dst_access_mask)
                .src_queue_family_index(*src_queue_family)
                .dst_queue_family_index(*dst_queue_family)
                .build()
        })
    }

    /// Returns the buffer used in this op
    pub fn get_buffer(&self) -> Buffer {
        self.buffer
    }

    /// Returns a barrier which needs to be submitted by the transfer engine before using the buffer.
    fn make_transfer_barrier(&self, src_stage_mask: vk::PipelineStageFlags2, src_access_mask: vk::AccessFlags2) -> Option<vk::BufferMemoryBarrier2> {
        self.dst_info.as_ref().map(|(dst_stage_mask, dst_access_mask)| {
            let mut barrier = vk::BufferMemoryBarrier2::builder()
                .buffer(self.buffer.get_handle())
                .offset(self.offset)
                .size(self.size)
                .src_stage_mask(src_stage_mask)
                .src_access_mask(src_access_mask);

            if let Some((src_queue_family, dst_queue_family)) = &self.queue_info {
                barrier = barrier
                    .src_queue_family_index(*src_queue_family)
                    .dst_queue_family_index(*dst_queue_family);
            } else {
                barrier = barrier
                    .dst_stage_mask(*dst_stage_mask)
                    .dst_access_mask(*dst_access_mask);
            }

            barrier.build()
        })
    }
}

#[derive(Copy, Clone, Debug)]
pub struct ImageAvailabilityOp {
    image: Image,
    aspect_mask: vk::ImageAspectFlags,
    /// The layout the image will be in after a acquire or the layout the image should be in
    /// before a release on the transfer queue.
    local_layout: vk::ImageLayout,
    barrier: Option<vk::ImageMemoryBarrier2>,
}

impl ImageAvailabilityOp {
    /// Returns the generated barrier
    pub fn get_barrier(&self) -> Option<&vk::ImageMemoryBarrier2> {
        self.barrier.as_ref()
    }

    /// Returns the image used in this op
    pub fn get_image(&self) -> Image {
        self.image
    }
}

// vk::ImageMemoryBarrier2 does not implement send so we have to do it here
unsafe impl Send for ImageAvailabilityOp {
}

pub struct StagingMemory<'a> {
    transfer: &'a Transfer,
    memory: &'a mut [u8],
    memory_id: UUID,
    buffer_offset: vk::DeviceSize,
}

impl<'a> StagingMemory<'a> {
    /// Returns a slice to the staging memory range
    pub fn get_memory(&mut self) -> &mut [u8] {
        &mut self.memory
    }

    /// Writes the data stored in the slice to the memory and returns the number of bytes written.
    /// If the data does not fit into the available memory range [`None`] is returned.
    pub fn write<T: Copy>(&mut self, data: &[T]) -> Option<usize> {
        self.write_offset(data, 0)
    }

    /// Writes the data stored in the slice to the memory at the specified offset and returns the
    /// number of bytes written.
    /// If the data does not fit into the available memory range [`None`] is returned.
    pub fn write_offset<T: Copy>(&mut self, data: &[T], offset: usize) -> Option<usize> {
        let byte_count = data.len() * std::mem::size_of::<T>();
        if (offset + byte_count) > self.memory.len() {
            return None;
        }

        let src = unsafe {
            std::slice::from_raw_parts(data.as_ptr() as *const u8, byte_count)
        };
        let dst = &mut self.memory[offset..byte_count];
        dst.copy_from_slice(src);

        Some(byte_count)
    }

    pub fn read<T: Copy>(&self, data: &mut [T]) -> Result<(), ()> {
        self.read_offset(data, 0)
    }

    pub fn read_offset<T: Copy>(&self, data: &mut [T], offset: usize) -> Result<(), ()> {
        let byte_count = data.len() * std::mem::size_of::<T>();
        if (offset + byte_count) > self.memory.len() {
            return Err(());
        }

        let src = &self.memory[offset..byte_count];
        let dst = unsafe {
            std::slice::from_raw_parts_mut(data.as_ptr() as *mut u8, byte_count)
        };
        dst.copy_from_slice(src);

        Ok(())
    }

    pub fn copy_to_buffer<T: Into<BufferId>>(&mut self, dst_buffer: T, mut ranges: BufferTransferRanges) {
        ranges.add_src_offset(self.buffer_offset);
        let task = Task::BufferTransfer(BufferTransfer {
            src_buffer: BufferId::from_raw(self.memory_id),
            dst_buffer: dst_buffer.into(),
            ranges
        });
        self.transfer.share.push_task(task);
    }

    pub fn copy_from_buffer<T: Into<BufferId>>(&mut self, src_buffer: T, mut ranges: BufferTransferRanges) {
        ranges.add_dst_offset(self.buffer_offset);
        let task = Task::BufferTransfer(BufferTransfer {
            src_buffer: src_buffer.into(),
            dst_buffer: BufferId::from_raw(self.memory_id),
            ranges
        });
        self.transfer.share.push_task(task);
    }

    pub fn copy_to_image<T: Into<ImageId>>(&mut self, dst_image: T, mut ranges: BufferImageTransferRanges) {
        ranges.add_buffer_offset(self.buffer_offset);
        let task = Task::BufferToImageTransfer(BufferToImageTransfer {
            src_buffer: BufferId::from_raw(self.memory_id),
            dst_image: dst_image.into(),
            ranges
        });
        self.transfer.share.push_task(task);
    }

    pub fn flush(&self) {

    }
}

impl<'a> Drop for StagingMemory<'a> {
    fn drop(&mut self) {
        self.transfer.share.push_task(Task::StagingRelease(self.memory_id));
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BufferTransferRange {
    pub src_offset: vk::DeviceSize,
    pub dst_offset: vk::DeviceSize,
    pub size: vk::DeviceSize,
}

impl BufferTransferRange {
    pub fn new(src_offset: vk::DeviceSize, dst_offset: vk::DeviceSize, size: vk::DeviceSize) -> Self {
        Self {
            src_offset,
            dst_offset,
            size
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BufferTransferRanges {
    One(BufferTransferRange),
    Multiple(Box<[BufferTransferRange]>),
}

impl BufferTransferRanges {
    pub fn new_single(src_offset: vk::DeviceSize, dst_offset: vk::DeviceSize, size: vk::DeviceSize) -> Self {
        Self::One(BufferTransferRange::new(src_offset, dst_offset, size))
    }

    pub fn add_src_offset(&mut self, src_offset: vk::DeviceSize) {
        match self {
            BufferTransferRanges::One(range) => range.src_offset += src_offset,
            BufferTransferRanges::Multiple(ranges) => {
                for range in ranges.as_mut() {
                    range.src_offset += src_offset;
                }
            }
        }
    }

    pub fn add_dst_offset(&mut self, dst_offset: vk::DeviceSize) {
        match self {
            BufferTransferRanges::One(range) => range.dst_offset += dst_offset,
            BufferTransferRanges::Multiple(ranges) => {
                for range in ranges.as_mut() {
                    range.dst_offset += dst_offset;
                }
            }
        }
    }

    pub fn as_slice(&self) -> &[BufferTransferRange] {
        match self {
            BufferTransferRanges::One(range) => std::slice::from_ref(range),
            BufferTransferRanges::Multiple(ranges) => ranges.as_ref(),
        }
    }
}

#[derive(Debug)]
pub struct BufferTransfer {
    pub src_buffer: BufferId,
    pub dst_buffer: BufferId,
    pub ranges: BufferTransferRanges,
}

impl BufferTransfer {
    pub fn new_single_range<S: Into<BufferId>, D: Into<BufferId>>(
        src_buffer: S,
        src_offset: vk::DeviceSize,
        dst_buffer: D,
        dst_offset: vk::DeviceSize,
        size: vk::DeviceSize
    ) -> Self {
        Self {
            src_buffer: src_buffer.into(),
            dst_buffer: dst_buffer.into(),
            ranges: BufferTransferRanges::new_single(src_offset, dst_offset, size)
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct BufferImageTransferRange {
    pub buffer_offset: vk::DeviceSize,
    pub buffer_row_length: u32,
    pub buffer_image_height: u32,
    pub image_aspect_mask: vk::ImageAspectFlags,
    pub image_mip_level: u32,
    pub image_base_array_layer: u32,
    pub image_layer_count: u32,
    pub image_offset: Vec3i32,
    pub image_extent: Vec3u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum BufferImageTransferRanges {
    One(BufferImageTransferRange),
    Multiple(Box<[BufferImageTransferRange]>),
}

impl BufferImageTransferRanges {
    pub fn as_slice(&self) -> &[BufferImageTransferRange] {
        match self {
            Self::One(range) => std::slice::from_ref(range),
            Self::Multiple(ranges) => ranges.as_ref(),
        }
    }

    pub fn add_buffer_offset(&mut self, offset: vk::DeviceSize) {
        match self {
            BufferImageTransferRanges::One(range) => range.buffer_offset += offset,
            BufferImageTransferRanges::Multiple(ranges) => {
                for range in ranges.as_mut() {
                    range.buffer_offset += offset;
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct BufferToImageTransfer {
    src_buffer: BufferId,
    dst_image: ImageId,
    ranges: BufferImageTransferRanges,
}

#[derive(Debug)]
pub struct ImageToBufferTransfer {
    src_image: ImageId,
    dst_buffer: BufferId,
    ranges: BufferImageTransferRanges,
}

#[cfg(test)]
mod tests {
    use crate::vk::test::make_headless_instance_device;
    use super::*;

    fn create_test_buffer(device: &DeviceEnvironment, size: usize) -> Buffer {
        let info = vk::BufferCreateInfo::builder()
            .size(size as vk::DeviceSize)
            .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);

        let buffer = unsafe {
            device.vk().create_buffer(&info, None)
        }.unwrap();

        let allocation = device.get_allocator().allocate_buffer_memory(buffer, &AllocationStrategy::AutoGpuOnly).unwrap();

        unsafe {
            device.vk().bind_buffer_memory(buffer, allocation.memory(), allocation.offset())
        }.unwrap();

        Buffer::new(buffer)
    }

    #[test]
    fn test_buffer_copy() {
        env_logger::init();

        let (_, device) = make_headless_instance_device();

        let buffer = create_test_buffer(&device, 1024);
        let transfer = device.get_transfer();

        let data: Vec<_> = (0u32..16u32).collect();
        let byte_size = data.len() * std::mem::size_of::<u32>();

        let op = transfer.prepare_buffer_acquire(buffer, None);
        transfer.acquire_buffer(op, SemaphoreOps::None).unwrap();

        let mut write_mem = transfer.request_staging_memory(byte_size);
        write_mem.write(data.as_slice());
        write_mem.copy_to_buffer(buffer, BufferTransferRanges::new_single(0, 0, byte_size as vk::DeviceSize));

        let mut dst_data = Vec::new();
        dst_data.resize(data.len(), 0u32);

        let mut read_mem = transfer.request_staging_memory(byte_size);
        read_mem.copy_from_buffer(buffer, BufferTransferRanges::new_single(0, 0, byte_size as vk::DeviceSize));

        let op = transfer.prepare_buffer_release(buffer, None);
        let id = transfer.release_buffer(op).unwrap();
        transfer.flush(id);

        transfer.wait_for_complete(id);
        read_mem.read(dst_data.as_mut_slice()).unwrap();

        unsafe {
            device.vk().destroy_buffer(buffer.get_handle(), None)
        };

        assert_eq!(data, dst_data);
    }
}