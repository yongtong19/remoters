#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <winioctl.h>
#include <setupapi.h>

#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <string>

namespace {

struct RemotersGamepadState {
    uint8_t slot;
    uint8_t connected;
    uint16_t buttons;
    uint8_t left_trigger;
    uint8_t right_trigger;
    int16_t left_x;
    int16_t left_y;
    int16_t right_x;
    int16_t right_y;
};

struct XInputGamepad {
    WORD wButtons;
    BYTE bLeftTrigger;
    BYTE bRightTrigger;
    SHORT sThumbLX;
    SHORT sThumbLY;
    SHORT sThumbRX;
    SHORT sThumbRY;
};

struct XInputState {
    DWORD dwPacketNumber;
    XInputGamepad Gamepad;
};

using XInputGetStateFn = DWORD(WINAPI *)(DWORD dwUserIndex, XInputState *pState);

std::once_flag g_xinput_once;
HMODULE g_xinput_module = nullptr;
XInputGetStateFn g_xinput_get_state = nullptr;

void init_xinput() {
    static const wchar_t *kDlls[] = {L"xinput1_4.dll", L"xinput1_3.dll", L"xinput9_1_0.dll"};
    for (const wchar_t *name : kDlls) {
        HMODULE mod = LoadLibraryW(name);
        if (!mod) {
            continue;
        }
        // Ordinal 100 in xinput1_4/xinput1_3 is XInputGetStateEx, which includes the Guide button.
        auto fn = reinterpret_cast<XInputGetStateFn>(
            GetProcAddress(mod, reinterpret_cast<LPCSTR>(static_cast<uintptr_t>(100))));
        if (!fn) {
            fn = reinterpret_cast<XInputGetStateFn>(GetProcAddress(mod, "XInputGetState"));
        }
        if (fn) {
            g_xinput_module = mod;
            g_xinput_get_state = fn;
            return;
        }
        FreeLibrary(mod);
    }
}

// Nefarius ViGEmBus interface GUID: {96E42B22-F5E9-42F8-B043-ED0F932F014F}
const GUID kVigemBusGuid{
    0x96E42B22, 0xF5E9, 0x42F8, {0xB0, 0x43, 0xED, 0x0F, 0x93, 0x2F, 0x01, 0x4F}};

constexpr ULONG kVigemCommonVersion = 0x0001;
constexpr ULONG kVigemTargetXbox360Wired = 0;
constexpr ULONG kVigemMaxSerial = 16;

// From Nefarius ViGEmClient/include/ViGEm/km/BusShared.h (IOCTL_VIGEM_BASE = 0x801):
//   PLUGIN_TARGET = +0x000 (0x801), UNPLUG_TARGET = +0x001 (0x802),
//   CHECK_VERSION = +0x002 (0x803), WAIT_DEVICE_READY = +0x003 (0x804),
//   XUSB_SUBMIT_REPORT = +0x201 (0xA02).
constexpr DWORD kIoctlVigemPluginTarget =
    CTL_CODE(FILE_DEVICE_BUS_EXTENDER, 0x801, METHOD_BUFFERED, FILE_WRITE_DATA);
constexpr DWORD kIoctlVigemUnplugTarget =
    CTL_CODE(FILE_DEVICE_BUS_EXTENDER, 0x802, METHOD_BUFFERED, FILE_WRITE_DATA);
constexpr DWORD kIoctlVigemCheckVersion =
    CTL_CODE(FILE_DEVICE_BUS_EXTENDER, 0x803, METHOD_BUFFERED, FILE_WRITE_DATA);
constexpr DWORD kIoctlVigemWaitDeviceReady =
    CTL_CODE(FILE_DEVICE_BUS_EXTENDER, 0x804, METHOD_BUFFERED, FILE_WRITE_DATA);
constexpr DWORD kIoctlXusbSubmitReport =
    CTL_CODE(FILE_DEVICE_BUS_EXTENDER, 0x801 + 0x201, METHOD_BUFFERED, FILE_WRITE_DATA);

struct VigemPluginTarget {
    ULONG Size;
    ULONG SerialNo;
    ULONG TargetType;
    USHORT VendorId;
    USHORT ProductId;
};
static_assert(sizeof(VigemPluginTarget) == 16);

struct VigemUnplugTarget {
    ULONG Size;
    ULONG SerialNo;
};
static_assert(sizeof(VigemUnplugTarget) == 8);

struct VigemCheckVersion {
    ULONG Size;
    ULONG Version;
};
static_assert(sizeof(VigemCheckVersion) == 8);

struct VigemWaitDeviceReady {
    ULONG Size;
    ULONG SerialNo;
};
static_assert(sizeof(VigemWaitDeviceReady) == 8);

struct XusbReport {
    USHORT wButtons;
    BYTE bLeftTrigger;
    BYTE bRightTrigger;
    SHORT sThumbLX;
    SHORT sThumbLY;
    SHORT sThumbRX;
    SHORT sThumbRY;
};
static_assert(sizeof(XusbReport) == 12);

struct XusbSubmitReport {
    ULONG Size;
    ULONG SerialNo;
    XusbReport Report;
};
static_assert(sizeof(XusbSubmitReport) == 20);

bool ioctl_sync(HANDLE bus, DWORD code, void *in_buf, DWORD in_size, DWORD &last_error) {
    HANDLE event = CreateEventW(nullptr, TRUE, FALSE, nullptr);
    if (!event) {
        last_error = GetLastError();
        return false;
    }
    OVERLAPPED ol{};
    ol.hEvent = event;
    DWORD transferred = 0;
    BOOL ok = DeviceIoControl(bus, code, in_buf, in_size, nullptr, 0, &transferred, &ol);
    if (!ok) {
        DWORD err = GetLastError();
        if (err == ERROR_IO_PENDING) {
            ok = GetOverlappedResult(bus, &ol, &transferred, TRUE);
            if (!ok) {
                err = GetLastError();
            }
        }
        if (!ok) {
            last_error = err;
            CloseHandle(event);
            return false;
        }
    }
    CloseHandle(event);
    last_error = ERROR_SUCCESS;
    return true;
}

HANDLE open_vigem_bus(DWORD &out_error, std::string &out_message) {
    HDEVINFO devices = SetupDiGetClassDevsW(
        &kVigemBusGuid, nullptr, nullptr, DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (devices == INVALID_HANDLE_VALUE) {
        out_error = GetLastError();
        out_message = "SetupDiGetClassDevsW failed for ViGEmBus.";
        return INVALID_HANDLE_VALUE;
    }
    const auto cleanup = std::unique_ptr<void, decltype(&SetupDiDestroyDeviceInfoList)>(
        devices, SetupDiDestroyDeviceInfoList);

    out_error = ERROR_FILE_NOT_FOUND;
    out_message = "ViGEmBus driver not found.";

    for (DWORD index = 0;; ++index) {
        SP_DEVICE_INTERFACE_DATA item{sizeof(item)};
        if (!SetupDiEnumDeviceInterfaces(devices, nullptr, &kVigemBusGuid, index, &item)) {
            break;
        }
        DWORD required = 0;
        SetupDiGetDeviceInterfaceDetailW(devices, &item, nullptr, 0, &required, nullptr);
        if (required < sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W)) {
            continue;
        }
        auto storage = std::make_unique<std::byte[]>(required);
        auto *detail = reinterpret_cast<SP_DEVICE_INTERFACE_DETAIL_DATA_W *>(storage.get());
        detail->cbSize = sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W);
        if (!SetupDiGetDeviceInterfaceDetailW(devices, &item, detail, required, nullptr, nullptr)) {
            out_error = GetLastError();
            continue;
        }
        HANDLE bus = CreateFileW(
            detail->DevicePath,
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            nullptr,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            nullptr);
        if (bus == INVALID_HANDLE_VALUE) {
            out_error = GetLastError();
            out_message = "Failed to open ViGEmBus device handle.";
            continue;
        }

        VigemCheckVersion ver{};
        ver.Size = sizeof(ver);
        ver.Version = kVigemCommonVersion;
        DWORD ioctl_err = ERROR_SUCCESS;
        if (!ioctl_sync(bus, kIoctlVigemCheckVersion, &ver, ver.Size, ioctl_err)) {
            CloseHandle(bus);
            out_error = ioctl_err;
            out_message = "ViGEmBus version check failed.";
            continue;
        }
        out_error = ERROR_SUCCESS;
        out_message.clear();
        return bus;
    }
    return INVALID_HANDLE_VALUE;
}

bool vigem_plug_slot(HANDLE bus, const ULONG used_serials[4], ULONG &out_serial, DWORD &out_err) {
    for (ULONG serial = 1; serial <= kVigemMaxSerial; ++serial) {
        bool in_use = false;
        for (int i = 0; i < 4; ++i) {
            if (used_serials[i] == serial) {
                in_use = true;
                break;
            }
        }
        if (in_use) {
            continue;
        }

        VigemPluginTarget plugin{};
        plugin.Size = sizeof(plugin);
        plugin.SerialNo = serial;
        plugin.TargetType = kVigemTargetXbox360Wired;
        plugin.VendorId = 0;
        plugin.ProductId = 0;

        DWORD err = ERROR_SUCCESS;
        if (!ioctl_sync(bus, kIoctlVigemPluginTarget, &plugin, plugin.Size, err)) {
            out_err = err;
            continue;
        }

        VigemWaitDeviceReady wait{};
        wait.Size = sizeof(wait);
        wait.SerialNo = serial;
        DWORD wait_err = ERROR_SUCCESS;
        if (!ioctl_sync(bus, kIoctlVigemWaitDeviceReady, &wait, wait.Size, wait_err)) {
            // Pre-v1.17 ViGEmBus returns ERROR_INVALID_PARAMETER for WAIT_DEVICE_READY.
            if (wait_err != ERROR_INVALID_PARAMETER) {
                VigemUnplugTarget unplug{sizeof(VigemUnplugTarget), serial};
                DWORD dummy = 0;
                ioctl_sync(bus, kIoctlVigemUnplugTarget, &unplug, unplug.Size, dummy);
                out_err = wait_err;
                return false;
            }
        }
        out_serial = serial;
        out_err = ERROR_SUCCESS;
        return true;
    }
    return false;
}

bool vigem_unplug_serial(HANDLE bus, ULONG serial) {
    if (bus == INVALID_HANDLE_VALUE || serial == 0) {
        return true;
    }
    VigemUnplugTarget unplug{};
    unplug.Size = sizeof(unplug);
    unplug.SerialNo = serial;
    DWORD err = ERROR_SUCCESS;
    return ioctl_sync(bus, kIoctlVigemUnplugTarget, &unplug, unplug.Size, err);
}

bool vigem_submit_xusb(HANDLE bus, ULONG serial, const RemotersGamepadState &state, DWORD &out_err) {
    XusbSubmitReport submit{};
    submit.Size = sizeof(submit);
    submit.SerialNo = serial;
    submit.Report.wButtons = state.buttons;
    submit.Report.bLeftTrigger = state.left_trigger;
    submit.Report.bRightTrigger = state.right_trigger;
    submit.Report.sThumbLX = state.left_x;
    submit.Report.sThumbLY = state.left_y;
    submit.Report.sThumbRX = state.right_x;
    submit.Report.sThumbRY = state.right_y;
    return ioctl_sync(bus, kIoctlXusbSubmitReport, &submit, submit.Size, out_err);
}

// ViGEmBus returns STATUS_NO_MORE_ENTRIES (Win32 ERROR_NO_MORE_ITEMS = 259) when xusb22.sys
// has not yet queued its next Interrupt IN URB (e.g. right after plug-in or rapid updates).
bool vigem_submit_xusb_retry(
    HANDLE bus,
    ULONG serial,
    const RemotersGamepadState &state,
    int max_attempts,
    DWORD sleep_ms,
    DWORD &out_err
) {
    for (int attempt = 0; attempt < max_attempts; ++attempt) {
        if (vigem_submit_xusb(bus, serial, state, out_err)) {
            return true;
        }
        if (out_err != ERROR_NO_MORE_ITEMS) {
            return false;
        }
        if (attempt + 1 < max_attempts) {
            Sleep(sleep_ms);
        }
    }
    return false;
}

struct VigemHost {
    HANDLE bus = INVALID_HANDLE_VALUE;
    ULONG serials[4]{0, 0, 0, 0};
    bool permanently_unavailable = false;
    std::string last_error;

    ~VigemHost() { reset(); }

    void reset() {
        if (bus != INVALID_HANDLE_VALUE) {
            for (ULONG &serial : serials) {
                if (serial != 0) {
                    vigem_unplug_serial(bus, serial);
                    serial = 0;
                }
            }
            CloseHandle(bus);
            bus = INVALID_HANDLE_VALUE;
        } else {
            for (ULONG &serial : serials) {
                serial = 0;
            }
        }
    }

    bool ensure_bus() {
        if (bus != INVALID_HANDLE_VALUE) {
            return true;
        }
        if (permanently_unavailable) {
            return false;
        }
        DWORD err = ERROR_SUCCESS;
        std::string msg;
        bus = open_vigem_bus(err, msg);
        if (bus == INVALID_HANDLE_VALUE) {
            permanently_unavailable = true;
            char buf[256];
            std::snprintf(buf, sizeof(buf), "%s (Win32 error %lu)", msg.c_str(), err);
            last_error = buf;
            return false;
        }
        return true;
    }

    bool apply(const RemotersGamepadState &state) {
        if (state.slot >= 4) {
            return true;
        }
        const uint8_t slot = state.slot;
        if (!state.connected) {
            if (bus != INVALID_HANDLE_VALUE && serials[slot] != 0) {
                vigem_unplug_serial(bus, serials[slot]);
                serials[slot] = 0;
            }
            return true;
        }
        if (!ensure_bus()) {
            return false;
        }
        bool just_plugged = false;
        if (serials[slot] == 0) {
            ULONG new_serial = 0;
            DWORD err = ERROR_SUCCESS;
            if (!vigem_plug_slot(bus, serials, new_serial, err)) {
                char buf[160];
                std::snprintf(
                    buf, sizeof(buf), "ViGEmBus plug-in failed for slot %u (error %lu).", slot, err);
                last_error = buf;
                return false;
            }
            serials[slot] = new_serial;
            just_plugged = true;
        }
        DWORD err = ERROR_SUCCESS;
        const int attempts = just_plugged ? 30 : 4;
        const DWORD delay_ms = just_plugged ? 10 : 2;
        if (!vigem_submit_xusb_retry(bus, serials[slot], state, attempts, delay_ms, err)) {
            if (err == ERROR_NO_MORE_ITEMS) {
                // Transient backpressure while xusb22.sys re-queues its Interrupt IN URB.
                return true;
            }
            char buf[160];
            std::snprintf(
                buf, sizeof(buf), "ViGEmBus report failed for slot %u (error %lu).", slot, err);
            last_error = buf;
            if (err == ERROR_DEV_NOT_EXIST || err == ERROR_INVALID_HANDLE) {
                reset();
            }
            return false;
        }
        return true;
    }
};

} // namespace

extern "C" {

void remoters_win_gamepad_poll(RemotersGamepadState out_states[4]) {
    if (!out_states) {
        return;
    }
    std::call_once(g_xinput_once, init_xinput);
    for (DWORD i = 0; i < 4; ++i) {
        std::memset(&out_states[i], 0, sizeof(RemotersGamepadState));
        out_states[i].slot = static_cast<uint8_t>(i);
        if (!g_xinput_get_state) {
            continue;
        }
        XInputState state{};
        if (g_xinput_get_state(i, &state) == ERROR_SUCCESS) {
            out_states[i].connected = 1;
            out_states[i].buttons = state.Gamepad.wButtons;
            out_states[i].left_trigger = state.Gamepad.bLeftTrigger;
            out_states[i].right_trigger = state.Gamepad.bRightTrigger;
            out_states[i].left_x = state.Gamepad.sThumbLX;
            out_states[i].left_y = state.Gamepad.sThumbLY;
            out_states[i].right_x = state.Gamepad.sThumbRX;
            out_states[i].right_y = state.Gamepad.sThumbRY;
        }
    }
}

void *remoters_win_vigem_create(void) { return new VigemHost(); }

bool remoters_win_vigem_apply(void *ptr, const RemotersGamepadState *state) {
    if (!ptr || !state) {
        return false;
    }
    return static_cast<VigemHost *>(ptr)->apply(*state);
}

void remoters_win_vigem_reset(void *ptr) {
    if (!ptr) {
        return;
    }
    static_cast<VigemHost *>(ptr)->reset();
}

void remoters_win_vigem_destroy(void *ptr) {
    delete static_cast<VigemHost *>(ptr);
}

size_t remoters_win_vigem_last_error(void *ptr, char *buf, size_t cap) {
    if (!ptr || !buf || cap == 0) {
        return 0;
    }
    const auto &err = static_cast<VigemHost *>(ptr)->last_error;
    const size_t n = err.size() < (cap - 1) ? err.size() : (cap - 1);
    std::memcpy(buf, err.data(), n);
    buf[n] = '\0';
    return n;
}

int32_t remoters_win_vigem_probe(char *err_buf, size_t err_cap) {
    DWORD err = ERROR_SUCCESS;
    std::string msg;
    HANDLE bus = open_vigem_bus(err, msg);
    if (bus == INVALID_HANDLE_VALUE) {
        if (err_buf && err_cap > 0) {
            std::snprintf(err_buf, err_cap, "%s (Win32 error %lu)", msg.c_str(), err);
        }
        return (err == ERROR_FILE_NOT_FOUND || err == ERROR_DEVICE_NOT_CONNECTED) ? 1 : -1;
    }

    ULONG used[4]{0, 0, 0, 0};
    ULONG serial = 0;
    if (!vigem_plug_slot(bus, used, serial, err)) {
        if (err_buf && err_cap > 0) {
            std::snprintf(err_buf, err_cap, "vigem_plug_slot failed (error %lu)", err);
        }
        CloseHandle(bus);
        return -1;
    }

    RemotersGamepadState state{};
    state.slot = 0;
    state.connected = 1;
    state.buttons = 0x1000; // A
    state.left_trigger = 128;
    state.left_x = 12000;
    state.left_y = -12000;
    bool ok = vigem_submit_xusb_retry(bus, serial, state, 50, 10, err);
    if (ok) {
        RemotersGamepadState neutral{};
        neutral.slot = 0;
        neutral.connected = 1;
        ok = vigem_submit_xusb_retry(bus, serial, neutral, 50, 10, err);
    }
    if (!ok && err_buf && err_cap > 0) {
        std::snprintf(err_buf, err_cap, "vigem_submit_xusb failed (error %lu)", err);
    }
    vigem_unplug_serial(bus, serial);
    CloseHandle(bus);
    return ok ? 0 : -1;
}

} // extern "C"
