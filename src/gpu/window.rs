use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_metal::{MTLDevice, MTLPixelFormat};
use objc2_quartz_core::CAMetalLayer;
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::error::{CockpitError, Result};

pub struct CockpitWindow {
    pub window: Window,
    pub metal_layer: Retained<CAMetalLayer>,
}

impl CockpitWindow {
    pub fn from_window(
        window: Window,
        device: &ProtocolObject<dyn MTLDevice>,
    ) -> Result<Self> {
        let handle = window
            .window_handle()
            .map_err(|e| CockpitError::Render(format!("failed to get window handle: {e}")))?;

        let ns_view_ptr: NonNull<std::ffi::c_void> = match handle.as_raw() {
            RawWindowHandle::AppKit(h) => h.ns_view,
            _ => return Err(CockpitError::Render("expected AppKit window handle".into())),
        };

        let raw_layer = unsafe { raw_window_metal::Layer::from_ns_view(ns_view_ptr) };
        let layer_ptr: *mut CAMetalLayer = raw_layer.into_raw().as_ptr().cast();
        let metal_layer: Retained<CAMetalLayer> = unsafe {
            Retained::from_raw(layer_ptr)
                .ok_or_else(|| CockpitError::Metal("null CAMetalLayer pointer".into()))?
        };

        metal_layer.setDevice(Some(device));
        metal_layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        metal_layer.setDisplaySyncEnabled(true);
        metal_layer.setFramebufferOnly(true);

        Ok(Self {
            window,
            metal_layer,
        })
    }
}
