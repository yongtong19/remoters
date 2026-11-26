use std::{net::Ipv4Addr, sync::OnceLock, thread, time::Duration};

use anyhow::{Result, anyhow};
use jni::{
    JNIEnv,
    objects::{JClass, JString},
    sys::{jint, jstring},
};
use remoters_core::{
    account::AccountClient,
    network::{
        self,
        discovery::{DiscoveryScanner, InterfaceAddress},
    },
};

use crate::{
    jni_bridge::{make_jstring, read_jstring},
    types::{
        DiscoveredHostDto, auth_success_json, devices_success_json, discovery_success_json,
        error_json, ok_json,
    },
};

const DEFAULT_DISCOVERY_TIMEOUT_MS: i32 = 1200;
const MIN_DISCOVERY_TIMEOUT_MS: i32 = 100;
const MAX_DISCOVERY_TIMEOUT_MS: i32 = 10_000;

static RUNTIME: OnceLock<Result<tokio::runtime::Runtime, String>> = OnceLock::new();

fn runtime() -> Result<&'static tokio::runtime::Runtime> {
    let res = RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| format!("Failed to initialize async runtime: {error}"))
    });
    match res {
        Ok(rt) => Ok(rt),
        Err(message) => Err(anyhow!("{message}")),
    }
}

fn local_interface_addresses() -> Vec<InterfaceAddress> {
    let mut interfaces: Vec<InterfaceAddress> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(v4) if network::is_allowed_ip(v4.ip) && !v4.ip.is_loopback() => {
                let broadcast = v4.broadcast.or_else(|| subnet_broadcast(v4.ip, v4.netmask));
                Some(InterfaceAddress {
                    ip: v4.ip,
                    broadcast,
                })
            }
            _ => None,
        })
        .collect();
    interfaces.sort_by_key(|iface| (iface.ip, iface.broadcast.is_none()));
    interfaces.dedup_by_key(|iface| iface.ip);
    interfaces.push(InterfaceAddress {
        ip: Ipv4Addr::LOCALHOST,
        broadcast: None,
    });
    interfaces
}

fn subnet_broadcast(ip: Ipv4Addr, netmask: Ipv4Addr) -> Option<Ipv4Addr> {
    let mask = u32::from(netmask);
    if mask == u32::MAX {
        return None;
    }
    Some(Ipv4Addr::from(u32::from(ip) | !mask))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_accountLogin<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    server_url: JString<'local>,
    username: JString<'local>,
    password: JString<'local>,
) -> jstring {
    let result = (|| -> Result<String> {
        let server_url = read_jstring(&mut env, &server_url)?;
        let username = read_jstring(&mut env, &username)?;
        let password = read_jstring(&mut env, &password)?;
        let client = AccountClient::new(&server_url)?;
        let session = runtime()?.block_on(client.login(&username, &password))?;
        Ok(auth_success_json(session))
    })();
    let json = match result {
        Ok(json) => json,
        Err(error) => error_json(error),
    };
    make_jstring(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_accountRegister<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    server_url: JString<'local>,
    username: JString<'local>,
    display_name: JString<'local>,
    password: JString<'local>,
) -> jstring {
    let result = (|| -> Result<String> {
        let server_url = read_jstring(&mut env, &server_url)?;
        let username = read_jstring(&mut env, &username)?;
        let display_name = if display_name.is_null() {
            String::new()
        } else {
            read_jstring(&mut env, &display_name)?
        };
        let password = read_jstring(&mut env, &password)?;
        let trimmed_display = display_name.trim();
        let display_opt = (!trimmed_display.is_empty()).then_some(trimmed_display);
        let client = AccountClient::new(&server_url)?;
        let session = runtime()?.block_on(client.register(&username, &password, display_opt))?;
        Ok(auth_success_json(session))
    })();
    let json = match result {
        Ok(json) => json,
        Err(error) => error_json(error),
    };
    make_jstring(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_accountListDevices<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    server_url: JString<'local>,
    token: JString<'local>,
) -> jstring {
    let result = (|| -> Result<String> {
        let server_url = read_jstring(&mut env, &server_url)?;
        let token = read_jstring(&mut env, &token)?;
        let client = AccountClient::new(&server_url)?;
        let devices = runtime()?.block_on(client.list_devices(&token))?;
        Ok(devices_success_json(devices))
    })();
    let json = match result {
        Ok(json) => json,
        Err(error) => error_json(error),
    };
    make_jstring(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_accountDeleteDevice<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    server_url: JString<'local>,
    token: JString<'local>,
    device_id: JString<'local>,
) -> jstring {
    let result = (|| -> Result<String> {
        let server_url = read_jstring(&mut env, &server_url)?;
        let token = read_jstring(&mut env, &token)?;
        let device_id = read_jstring(&mut env, &device_id)?;
        let client = AccountClient::new(&server_url)?;
        runtime()?.block_on(client.delete_device(&token, &device_id))?;
        Ok(ok_json())
    })();
    let json = match result {
        Ok(json) => json,
        Err(error) => error_json(error),
    };
    make_jstring(&mut env, &json)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_remoters_android_nativebridge_NativeBridge_discoverNearby<
    'local,
>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    timeout_ms: jint,
) -> jstring {
    let result = (|| -> Result<String> {
        let clamped_ms = if timeout_ms <= 0 {
            DEFAULT_DISCOVERY_TIMEOUT_MS
        } else {
            timeout_ms.clamp(MIN_DISCOVERY_TIMEOUT_MS, MAX_DISCOVERY_TIMEOUT_MS)
        } as u64;
        let scanner = DiscoveryScanner::start(local_interface_addresses())?;
        let wait = Duration::from_millis(clamped_ms);
        if clamped_ms >= 500 {
            let half = wait / 2;
            thread::sleep(half);
            scanner.probe();
            thread::sleep(wait.saturating_sub(half));
        } else {
            thread::sleep(wait);
        }
        let hosts = scanner
            .hosts()
            .into_iter()
            .map(|host| DiscoveredHostDto {
                instance_id: host.instance_id,
                address: host.address.to_string(),
                host_name: host.host_name,
                platform: host.platform,
                display_label: host.display_label,
                view_only: host.view_only,
            })
            .collect();
        Ok(discovery_success_json(hosts))
    })();
    let json = match result {
        Ok(json) => json,
        Err(error) => error_json(error),
    };
    make_jstring(&mut env, &json)
}
