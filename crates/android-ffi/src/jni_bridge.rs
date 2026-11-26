use anyhow::{Context, Result, ensure};
use jni::{
    JNIEnv,
    objects::{JObject, JString, JValue},
    sys::jstring,
};
use remoters_core::video::{Format, ReceivedFrame};

use crate::input::{codec_to_int, is_keyframe};

pub(crate) fn read_jstring(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Result<String> {
    ensure!(!value.is_null(), "Null string argument.");
    let s = env
        .get_string(value)
        .context("Failed to read Java string.")?;
    Ok(s.into())
}

pub(crate) fn make_jstring(env: &mut JNIEnv<'_>, text: &str) -> jstring {
    match env.new_string(text) {
        Ok(s) => s.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}

fn clear_java_exception(env: &mut JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

pub(crate) fn call_on_ready(
    env: &mut JNIEnv<'_>,
    callback: &JObject<'_>,
    view_only: bool,
    hdr: bool,
) {
    let _ = env.with_local_frame(8, |env| {
        if env
            .call_method(
                callback,
                "onReady",
                "(ZZ)V",
                &[
                    JValue::Bool(u8::from(view_only)),
                    JValue::Bool(u8::from(hdr)),
                ],
            )
            .is_err()
        {
            clear_java_exception(env);
        }
        Ok::<_, jni::errors::Error>(())
    });
}

pub(crate) fn call_on_video_frame(
    env: &mut JNIEnv<'_>,
    callback: &JObject<'_>,
    format: Format,
    frame: &ReceivedFrame,
) {
    let _ = env.with_local_frame(16, |env| {
        let Ok(bytes) = env.byte_array_from_slice(&frame.bytes) else {
            clear_java_exception(env);
            return Ok(());
        };
        let codec_int = codec_to_int(frame.codec);
        let keyframe = is_keyframe(frame.codec, &frame.bytes);
        let width = i32::from(format.width);
        let height = i32::from(format.height);
        let host_latency_us = i64::from(frame.host_latency_us);
        if env
            .call_method(
                callback,
                "onVideoFrame",
                "(IZII[BJ)V",
                &[
                    JValue::Int(codec_int),
                    JValue::Bool(u8::from(keyframe)),
                    JValue::Int(width),
                    JValue::Int(height),
                    JValue::Object(&bytes),
                    JValue::Long(host_latency_us),
                ],
            )
            .is_err()
        {
            clear_java_exception(env);
        }
        Ok::<_, jni::errors::Error>(())
    });
}

pub(crate) fn call_on_audio_pcm(
    env: &mut JNIEnv<'_>,
    callback: &JObject<'_>,
    sample_rate: u32,
    channels: u8,
    pcm: &[i16],
) {
    if pcm.is_empty() {
        return;
    }
    let mut le_bytes = Vec::with_capacity(pcm.len() * 2);
    for &sample in pcm {
        le_bytes.extend_from_slice(&sample.to_le_bytes());
    }
    let _ = env.with_local_frame(16, |env| {
        let Ok(bytes) = env.byte_array_from_slice(&le_bytes) else {
            clear_java_exception(env);
            return Ok(());
        };
        if env
            .call_method(
                callback,
                "onAudioPcm",
                "([BII)V",
                &[
                    JValue::Object(&bytes),
                    JValue::Int(sample_rate as i32),
                    JValue::Int(i32::from(channels)),
                ],
            )
            .is_err()
        {
            clear_java_exception(env);
        }
        Ok::<_, jni::errors::Error>(())
    });
}

pub(crate) fn call_on_stats_json(env: &mut JNIEnv<'_>, callback: &JObject<'_>, json: &str) {
    let _ = env.with_local_frame(8, |env| {
        let Ok(jjson) = env.new_string(json) else {
            clear_java_exception(env);
            return Ok(());
        };
        if env
            .call_method(
                callback,
                "onStatsJson",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&jjson)],
            )
            .is_err()
        {
            clear_java_exception(env);
        }
        Ok::<_, jni::errors::Error>(())
    });
}

pub(crate) fn call_on_closed(env: &mut JNIEnv<'_>, callback: &JObject<'_>, error: Option<&str>) {
    let _ = env.with_local_frame(8, |env| {
        let err_obj = match error {
            Some(msg) => match env.new_string(msg) {
                Ok(s) => JObject::from(s),
                Err(_) => {
                    clear_java_exception(env);
                    JObject::null()
                }
            },
            None => JObject::null(),
        };
        if env
            .call_method(
                callback,
                "onClosed",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&err_obj)],
            )
            .is_err()
        {
            clear_java_exception(env);
        }
        Ok::<_, jni::errors::Error>(())
    });
}
