//! A windowless wgpu device for CLI renders, screenshots and GPU tests.

use crate::Renderer;

pub struct Headless {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub renderer: Renderer,
    pub adapter_name: String,
}

/// The adapter for work without a window (renders, path tracing, GPU baking). Like the desktop app,
/// it honours `WGPU_ADAPTER_NAME` (part of the adapter's name, any case) and `WGPU_POWER_PREF`
/// (`high`, the default, `low` or `none`); the instance reads wgpu's other variables
/// (`WGPU_BACKEND`, `WGPU_ALLOW_UNDERLYING_NONCOMPLIANT_ADAPTER`, ...).
pub fn adapter() -> Result<wgpu::Adapter, String> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    if let Ok(name) = std::env::var("WGPU_ADAPTER_NAME") {
        let wanted = name.to_lowercase();
        let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()));
        if let Some(a) = adapters.into_iter().find(|a| a.get_info().name.to_lowercase().contains(&wanted)) {
            return Ok(a);
        }
        log::warn!("no GPU adapter matches WGPU_ADAPTER_NAME `{name}`; choosing by power preference");
    }
    let power_preference = wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance);
    pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference, ..Default::default() }))
        .map_err(|e| format!("no GPU adapter: {e}"))
}

/// The limits a headless device asks for: all the adapter has. The viewport draws with fragment
/// passes only, so an adapter without compute shaders (WebGL2-class, or the GL driver WSL2 offers
/// on an integrated GPU) still renders it; the GPU path tracer checks the device's limits itself.
fn required_limits(adapter: &wgpu::Limits) -> wgpu::Limits {
    adapter.clone()
}

impl Headless {
    /// Create a device on [`adapter`]. Errors when no GPU (or software) adapter exists.
    pub fn new() -> Result<Headless, String> {
        let adapter = adapter()?;
        let adapter_name = adapter.get_info().name;
        let desc = wgpu::DeviceDescriptor { label: Some("tc_headless"), required_limits: required_limits(&adapter.limits()), ..Default::default() };
        let (device, queue) = pollster::block_on(adapter.request_device(&desc)).map_err(|e| format!("no GPU device on {adapter_name}: {e}"))?;
        let renderer = Renderer::new(&device, &queue, wgpu::TextureFormat::Rgba8Unorm);
        Ok(Headless { device, queue, renderer, adapter_name })
    }

    /// Render a frame and read it back as RGBA8 (sRGB-encoded).
    pub fn render(&mut self, frame: &crate::Frame) -> Option<(u32, u32, Vec<u8>)> {
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("tc_headless") });
        self.renderer.prepare(&self.device, &self.queue, &mut enc, frame);
        self.queue.submit([enc.finish()]);
        self.renderer.read_view(&self.device, &self.queue, frame.view_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An adapter without compute shaders still gets a device for the viewport: asking for
    /// `Limits::default()` failed there with "Limit 'max_compute_workgroups_per_dimension' value
    /// 65535 is better than allowed 0" (WSL2's GL driver on an Intel GPU).
    #[test]
    fn an_adapter_without_compute_shaders_fits_the_device() {
        for adapter in [wgpu::Limits::downlevel_webgl2_defaults(), wgpu::Limits::downlevel_defaults(), wgpu::Limits::default()] {
            assert!(required_limits(&adapter).check_limits(&adapter), "{adapter:?}");
        }
    }
}
