//! Exportable image-ring allocation on wgpu's raw Vulkan device.
//!
//! wgpu 29 has no high-level external-memory API, so the ring images are
//! created with raw ash on the device wgpu already owns, allocated with
//! `VkExportMemoryAllocateInfo` (OPAQUE_FD), then wrapped back into wgpu via
//! `texture_from_raw` + `create_texture_from_hal`. Legal: wgpu-hal 29 enables
//! `VK_KHR_external_memory_fd` on its device whenever the driver supports it.
//!
//! Semaphore export: wgpu-hal 29 does **not** enable
//! `VK_KHR_external_semaphore_fd` itself (semaphore *creation* with an export
//! chain is core 1.1; only the fd-export entry point is extension-gated), so
//! `BevyProducer::new` appends it at device creation via Bevy's
//! `raw_vulkan_init` hook (see docs/upstream-watch.md item 2 for the upstream
//! fix). We still probe the entry point and exportability explicitly and fall
//! back to [`SyncKind::CpuBlocking`] loudly if either check fails — matching
//! VSE's never-assume-an-advertised-feature posture.

use std::os::fd::{FromRawFd, OwnedFd};

use ash::vk;
use tracing::warn;
use vse_external_frame::{ExternalImageDesc, RingFormat, SyncKind};

use crate::ProducerError;

pub(crate) fn wgpu_format(format: RingFormat) -> wgpu::TextureFormat {
    match format {
        RingFormat::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
        RingFormat::Bgra8UnormSrgb => wgpu::TextureFormat::Bgra8UnormSrgb,
        RingFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
    }
}

fn ash_format(format: RingFormat) -> vk::Format {
    vk::Format::from_raw(format.vk_format() as i32)
}

/// One allocated ring slot: raw Vulkan handles + the wgpu texture wrapping them.
///
/// PoC lifetime note: the raw image/memory are not destroyed on drop (the
/// producer lives for the whole session; destruction ordering against wgpu's
/// texture teardown is deferred to the dmabuf upgrade).
pub(crate) struct ExportedSlot {
    pub texture: wgpu::Texture,
    pub semaphore: Option<vk::Semaphore>,
}

pub(crate) struct ExportedRing {
    pub slots: Vec<ExportedSlot>,
    pub image_descs: Vec<ExternalImageDesc>,
    pub semaphore_fds: Vec<OwnedFd>,
    pub sync: SyncKind,
}

/// Allocate `ring_len` exportable images + ready semaphores on wgpu's device.
///
/// # Safety contract (internal)
/// Uses `Device::as_hal` for raw access; no wgpu resource is touched while the
/// hal guard is held except through documented wgpu-hal entry points.
pub(crate) fn allocate_ring(
    device: &wgpu::Device,
    format: RingFormat,
    extent: [u32; 2],
    ring_len: usize,
) -> Result<ExportedRing, ProducerError> {
    let mut slots = Vec::with_capacity(ring_len);
    let mut image_descs = Vec::with_capacity(ring_len);
    let mut semaphore_fds = Vec::with_capacity(ring_len);

    // Probe semaphore-fd export up front (see module docs).
    let sync = unsafe {
        let hal = device
            .as_hal::<wgpu::hal::api::Vulkan>()
            .ok_or_else(|| ProducerError::Setup("wgpu is not running on Vulkan".into()))?;
        probe_semaphore_export(&hal)
    };

    for slot_index in 0..ring_len {
        // SAFETY: raw resource creation on wgpu's device; handles are either
        // owned by the returned wgpu texture (image) or by the producer
        // (memory, semaphore). See module docs for the enablement argument.
        let (texture, image_desc, semaphore, sem_fd) = unsafe {
            let hal = device
                .as_hal::<wgpu::hal::api::Vulkan>()
                .expect("checked above");
            let raw = hal.raw_device();
            let instance = hal.shared_instance().raw_instance();
            let phys = hal.raw_physical_device();

            // --- Exportable image ---
            let mut ext_mem = vk::ExternalMemoryImageCreateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
            let image_ci = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(ash_format(format))
                .extent(vk::Extent3D {
                    width: extent[0],
                    height: extent[1],
                    depth: 1,
                })
                .mip_levels(1)
                .array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                // Byte-matches the importer's usage (core::external_frame):
                // COLOR_ATTACHMENT | TRANSFER_SRC | TRANSFER_DST.
                .usage(
                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .sharing_mode(vk::SharingMode::EXCLUSIVE)
                .initial_layout(vk::ImageLayout::UNDEFINED)
                .push_next(&mut ext_mem);
            let image = raw
                .create_image(&image_ci, None)
                .map_err(|e| ProducerError::Setup(format!("create_image[{slot_index}]: {e}")))?;

            // --- Dedicated exportable allocation ---
            let reqs = raw.get_image_memory_requirements(image);
            let mem_props = instance.get_physical_device_memory_properties(phys);
            let type_index = (0..mem_props.memory_type_count)
                .find(|&i| {
                    reqs.memory_type_bits & (1 << i) != 0
                        && mem_props.memory_types[i as usize]
                            .property_flags
                            .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
                })
                .ok_or_else(|| {
                    ProducerError::Setup("no device-local memory type for ring image".into())
                })?;
            let mut export = vk::ExportMemoryAllocateInfo::default()
                .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
            let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(reqs.size)
                .memory_type_index(type_index)
                .push_next(&mut export)
                .push_next(&mut dedicated);
            let memory = raw
                .allocate_memory(&alloc, None)
                .map_err(|e| ProducerError::Setup(format!("allocate_memory[{slot_index}]: {e}")))?;
            raw.bind_image_memory(image, memory, 0)
                .map_err(|e| ProducerError::Setup(format!("bind_image_memory[{slot_index}]: {e}")))?;

            // --- Export the memory fd (one per image; importer takes ownership) ---
            let fd_loader = ash::khr::external_memory_fd::Device::new(instance, raw);
            let fd = fd_loader
                .get_memory_fd(
                    &vk::MemoryGetFdInfoKHR::default()
                        .memory(memory)
                        .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
                )
                .map_err(|e| ProducerError::Setup(format!("get_memory_fd[{slot_index}]: {e}")))?;
            let memory_fd = OwnedFd::from_raw_fd(fd);

            // --- Wrap into wgpu ---
            let hal_texture = hal.texture_from_raw(
                image,
                &wgpu::hal::TextureDescriptor {
                    label: Some("vse-external-ring"),
                    size: wgpu::Extent3d {
                        width: extent[0],
                        height: extent[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu_format(format),
                    usage: wgpu::wgt::TextureUses::COLOR_TARGET
                        | wgpu::wgt::TextureUses::COPY_SRC
                        | wgpu::wgt::TextureUses::COPY_DST,
                    memory_flags: wgpu::hal::MemoryFlags::empty(),
                    view_formats: vec![],
                },
                // PoC: no drop callback — raw image/memory outlive the session.
                None,
                wgpu::hal::vulkan::TextureMemory::External,
            );
            drop(hal);
            let texture = device.create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("vse-external-ring"),
                    size: wgpu::Extent3d {
                        width: extent[0],
                        height: extent[1],
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu_format(format),
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::COPY_SRC
                        | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
            );

            // --- Ready semaphore (exported when supported) ---
            let hal = device
                .as_hal::<wgpu::hal::api::Vulkan>()
                .expect("checked above");
            let raw = hal.raw_device();
            let instance = hal.shared_instance().raw_instance();
            let (semaphore, sem_fd) = if sync == SyncKind::BinaryPerSlot {
                let mut export_sem = vk::ExportSemaphoreCreateInfo::default()
                    .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
                let sem_ci = vk::SemaphoreCreateInfo::default().push_next(&mut export_sem);
                let sem = raw.create_semaphore(&sem_ci, None).map_err(|e| {
                    ProducerError::Setup(format!("create_semaphore[{slot_index}]: {e}"))
                })?;
                let sem_loader = ash::khr::external_semaphore_fd::Device::new(instance, raw);
                let fd = sem_loader
                    .get_semaphore_fd(
                        &vk::SemaphoreGetFdInfoKHR::default()
                            .semaphore(sem)
                            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
                    )
                    .map_err(|e| {
                        ProducerError::Setup(format!("get_semaphore_fd[{slot_index}]: {e}"))
                    })?;
                (Some(sem), Some(OwnedFd::from_raw_fd(fd)))
            } else {
                (None, None)
            };

            (
                texture,
                ExternalImageDesc {
                    memory_fd,
                    allocation_size: reqs.size,
                    memory_type_index: type_index,
                    format,
                    extent,
                },
                semaphore,
                sem_fd,
            )
        };

        slots.push(ExportedSlot { texture, semaphore });
        image_descs.push(image_desc);
        if let Some(fd) = sem_fd {
            semaphore_fds.push(fd);
        }
    }

    Ok(ExportedRing {
        slots,
        image_descs,
        semaphore_fds,
        sync,
    })
}

/// Probe whether binary-semaphore OPAQUE_FD export will work on this device.
///
/// # Safety
/// `hal` must be a live wgpu-hal Vulkan device guard.
unsafe fn probe_semaphore_export(hal: &wgpu::hal::vulkan::Device) -> SyncKind {
    // Escape hatch for A/B timing comparisons and driver trouble: the two sync
    // modes are pixel-identical but pace the present loop differently under a
    // windowed compositor (CpuBlocking's per-frame stall reduces presents in
    // flight; see 01_bevy_ring_demo header).
    if std::env::var_os("VSE_BEVY_FORCE_CPU_BLOCKING").is_some() {
        warn!("VSE_BEVY_FORCE_CPU_BLOCKING set — skipping semaphore export probe");
        return SyncKind::CpuBlocking;
    }

    let instance = hal.shared_instance().raw_instance();
    let phys = hal.raw_physical_device();

    // 1. Driver-level exportability (instance-level query, fully conformant).
    let info = vk::PhysicalDeviceExternalSemaphoreInfo::default()
        .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
    let mut props = vk::ExternalSemaphoreProperties::default();
    instance.get_physical_device_external_semaphore_properties(phys, &info, &mut props);
    if !props
        .external_semaphore_features
        .contains(vk::ExternalSemaphoreFeatureFlags::EXPORTABLE)
    {
        warn!(
            "OPAQUE_FD semaphores not exportable on this driver — falling back to \
             CpuBlocking frame sync"
        );
        return SyncKind::CpuBlocking;
    }

    // 2. The entry point actually resolves on wgpu's device. wgpu-hal 29 does
    //    not enable VK_KHR_external_semaphore_fd itself; BevyProducer::new
    //    appends it via the raw_vulkan_init device callback. If that hook ever
    //    stops running (bevy feature regression, non-raw init path), the
    //    loader returns NULL here and we fall back rather than assume.
    let name = c"vkGetSemaphoreFdKHR";
    let fp = instance.get_device_proc_addr(hal.raw_device().handle(), name.as_ptr());
    if fp.is_none() {
        warn!(
            "vkGetSemaphoreFdKHR not resolvable on wgpu's device \
             (VK_KHR_external_semaphore_fd not enabled — raw_vulkan_init hook \
             did not run?) — falling back to CpuBlocking frame sync"
        );
        return SyncKind::CpuBlocking;
    }
    tracing::info!(
        "semaphore export probe: OPAQUE_FD exportable + vkGetSemaphoreFdKHR resolved \
         (VK_KHR_external_semaphore_fd enabled via raw_vulkan_init) — BinaryPerSlot"
    );
    SyncKind::BinaryPerSlot
}
