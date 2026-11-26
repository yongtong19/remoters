mod account;
mod input;
mod jni_bridge;
mod session;
mod types;

pub use account::{
    Java_com_remoters_android_nativebridge_NativeBridge_accountDeleteDevice,
    Java_com_remoters_android_nativebridge_NativeBridge_accountListDevices,
    Java_com_remoters_android_nativebridge_NativeBridge_accountLogin,
    Java_com_remoters_android_nativebridge_NativeBridge_accountRegister,
    Java_com_remoters_android_nativebridge_NativeBridge_discoverNearby,
};
pub use input::{
    build_stream_preference, clamp_point, codec_label, codec_to_int, is_keyframe, map_bitrate_mbps,
    map_gamepad_input, map_key_input, map_pointer_button, map_scroll_input,
};
pub use session::{
    Java_com_remoters_android_nativebridge_NativeBridge_sessionReleaseAll,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSendGamepad,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSendKey,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSendPointerButton,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSendPointerMove,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSendScroll,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSetAudioMuted,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionSetBitrate,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionStart,
    Java_com_remoters_android_nativebridge_NativeBridge_sessionStop,
};
