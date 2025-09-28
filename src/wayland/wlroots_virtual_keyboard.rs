#![cfg(feature = "wlroots_virtual_keyboard")]
use wayland_client::{
    protocol::{wl_seat::WlSeat, wl_registry::WlRegistry},
    Connection, Dispatch, QueueHandle, Proxy,
};
// Try the misc module first (most likely location in 0.32)
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::{ZwpVirtualKeyboardV1, Event as KeyboardEvent},
};


/// Wrapper for the virtual keyboard
pub struct VirtualKeyboard {
    vk_mgr: Option<ZwpVirtualKeyboardManagerV1>,
    vk: Option<ZwpVirtualKeyboardV1>,
    seat: Option<WlSeat>,
}

#[cfg(feature = "wlroots_virtual_keyboard")]
impl VirtualKeyboard {
    pub fn new() -> Self {
        Self {
            vk_mgr: None,
            vk: None,
            seat: None,
        }
    }

    /// Initialize Wayland virtual keyboard
    pub fn init(&mut self, conn: &Connection, qh: &QueueHandle<Self>) {
        let _registry = conn.display().get_registry(qh, ());
        // The actual binding happens in the Dispatch<WlRegistry> implementation
    }

    /// Create virtual keyboard instance once we have both manager and seat
    fn create_virtual_keyboard(&mut self, qh: &QueueHandle<Self>) {
        if let (Some(vk_mgr), Some(seat)) = (&self.vk_mgr, &self.seat) {
            self.vk = Some(vk_mgr.create_virtual_keyboard(seat, qh, ()));
        }
    }

    /// Send a synthetic key press and release
    pub fn send_key(&self, keycode: u32) {
        if let Some(vk) = &self.vk {
            let time = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u32;
            
            // Key press
            vk.key(time, keycode, 1); // 1 = pressed
            // Key release
            vk.key(time + 50, keycode, 0); // 0 = released
            vk.modifiers(0, 0, 0, 0); // clear modifiers
        }
    }
}

#[cfg(feature = "wlroots_virtual_keyboard")]
// Implement Dispatch traits for registry, virtual keyboard, etc.
impl Dispatch<WlRegistry, ()> for VirtualKeyboard {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wayland_client::protocol::wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wayland_client::protocol::wl_registry::Event::Global { name, interface, .. } = event {
            match &interface[..] {
                "zwp_virtual_keyboard_manager_v1" => {
                    state.vk_mgr = Some(registry.bind::<ZwpVirtualKeyboardManagerV1, _, _>(name, 1, qh, ()));
                    state.create_virtual_keyboard(qh);
                }
                "wl_seat" => {
                    if state.seat.is_none() {
                        state.seat = Some(registry.bind::<WlSeat, _, _>(name, 1, qh, ()));
                        state.create_virtual_keyboard(qh);
                    }
                }
                _ => {}
            }
        }
    }
}

// Dispatch implementations with correct signatures
impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for VirtualKeyboard {
    fn event(
        _state: &mut Self, 
        _proxy: &ZwpVirtualKeyboardManagerV1, 
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event, 
        _data: &(), 
        _conn: &Connection, 
        _qh: &QueueHandle<Self>
    ) {
        // Virtual keyboard manager events (usually none)
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for VirtualKeyboard {
    fn event(
        _state: &mut Self, 
        _proxy: &ZwpVirtualKeyboardV1, 
        _event: KeyboardEvent, 
        _data: &(), 
        _conn: &Connection, 
        _qh: &QueueHandle<Self>
    ) {
        // Virtual keyboard events (usually none for our use case)
    }
}

impl Dispatch<WlSeat, ()> for VirtualKeyboard {
    fn event(
        _state: &mut Self, 
        _proxy: &WlSeat, 
        _event: wayland_client::protocol::wl_seat::Event, 
        _data: &(), 
        _conn: &Connection, 
        _qh: &QueueHandle<Self>
    ) {
        // Seat events (capabilities, name, etc.)
    }
}
