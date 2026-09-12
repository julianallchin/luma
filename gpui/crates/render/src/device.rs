//! Device ownership shared by independent renderers, without their pipelines.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

/// A GPU device and submission queue, shared with the compositor when possible.
pub struct DeviceContext {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) lost: Arc<AtomicBool>,
    pub(crate) adopted: bool,
}

static SHARED: Mutex<Option<Arc<DeviceContext>>> = Mutex::new(None);

impl DeviceContext {
    /// Acquire the process-wide device without constructing stage pipelines.
    pub fn shared() -> anyhow::Result<Arc<Self>> {
        let mut slot = SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(context) = slot.as_ref().filter(|context| !context.is_lost()) {
            return Ok(context.clone());
        }
        let instance = wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            apply_limit_buckets: false,
            ..Default::default()
        }))?;
        // Subgroup ballots let the native haze pass write its lit-interval
        // cache header in one store per (workgroup, light); without them the
        // cache stays off and rendering is unchanged.
        let features =
            adapter.features() & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::SUBGROUP);
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("luma"),
                required_features: features,
                required_limits: wgpu::Limits {
                    // The lit-interval cache pool is one storage binding of
                    // roughly 4 B × blocks × shadow slots plus its payload
                    // table; the 128 MiB default caps it below 4K.
                    max_storage_buffer_binding_size: adapter
                        .limits()
                        .max_storage_buffer_binding_size,
                    max_buffer_size: adapter.limits().max_buffer_size,
                    // The cache adds three storage buffers to the native haze
                    // kernel's eight.
                    max_storage_buffers_per_shader_stage: adapter
                        .limits()
                        .max_storage_buffers_per_shader_stage,
                    ..wgpu::Limits::default().using_resolution(adapter.limits())
                },
                ..Default::default()
            }))?;
        let lost = Arc::new(AtomicBool::new(false));
        device.set_device_lost_callback({
            let lost = lost.clone();
            move |reason, message| {
                lost.store(true, Ordering::Relaxed);
                eprintln!("luma device lost ({reason:?}): {message}");
            }
        });
        let context = Arc::new(Self {
            device,
            queue,
            adapter,
            lost,
            adopted: false,
        });
        *slot = Some(context.clone());
        Ok(context)
    }

    /// Adopt the window's live device before any renderer is constructed.
    pub fn adopt(
        device: Arc<wgpu::Device>,
        queue: Arc<wgpu::Queue>,
        adapter: wgpu::Adapter,
        lost: Arc<AtomicBool>,
    ) {
        let mut slot = SHARED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|c| c.adopted && !c.is_lost()) {
            return;
        }
        *slot = Some(Arc::new(Self {
            device: (*device).clone(),
            queue: (*queue).clone(),
            adapter,
            lost,
            adopted: true,
        }));
    }

    /// Driver and hardware identity for benchmark evidence.
    pub fn adapter_info(&self) -> wgpu::AdapterInfo {
        self.adapter.get_info()
    }

    /// Whether resources must be rebuilt after driver/device loss.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Relaxed)
    }
}
