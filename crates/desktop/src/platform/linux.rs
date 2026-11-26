#[cfg(any(target_os = "linux", test))]
mod display_mode;
#[cfg(target_os = "linux")]
mod input;
#[cfg(any(target_os = "linux", test))]
mod keymap;
#[cfg(target_os = "linux")]
mod monitor;

#[cfg(any(target_os = "linux", test))]
#[allow(unused_imports)]
pub use display_mode::{
    LinuxDisplayInfo, LinuxDisplayMode, parse_gdctl_show, parse_xrandr_query, select_best_mode,
};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use display_mode::{
    gdctl_available, linux_primary_display_spec, query_linux_displays, restore_linux_display_mode,
    set_linux_display_mode, xrandr_available,
};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use input::{InputInjector, inject_key, move_pointer};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use keymap::vk_to_enigo_key;
#[cfg(any(target_os = "linux", test))]
#[allow(unused_imports)]
pub use keymap::{LinuxKey, linux_key_to_keysym, vk_to_linux_key};
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use monitor::{
    CursorDetector, display_label, list_displays, make_video_frame, monitor_physical_bounds,
    video_frame_dimensions,
};
