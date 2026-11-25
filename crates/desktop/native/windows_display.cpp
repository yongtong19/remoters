#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <winioctl.h>
#include <setupapi.h>
#include <objbase.h>

#include <algorithm>
#include <cstddef>
#include <cstdint>
#include <cstdio>
#include <cwchar>
#include <memory>
#include <system_error>
#include <thread>
#include <vector>

namespace {
// SudoVDA 0.2 user-mode wire interface used by Apollo. No driver is installed here.
const GUID interface_id{0xe5bcc234, 0x1e0c, 0x418a, {0xa0, 0xd4, 0xef, 0x8b, 0x75, 0x01, 0x41, 0x4d}};
constexpr DWORD add_display = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS);
constexpr DWORD remove_display = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x801, METHOD_BUFFERED, FILE_ANY_ACCESS);
constexpr DWORD get_watchdog = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x803, METHOD_BUFFERED, FILE_ANY_ACCESS);
constexpr DWORD ping = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x888, METHOD_BUFFERED, FILE_ANY_ACCESS);
constexpr DWORD get_version = CTL_CODE(FILE_DEVICE_UNKNOWN, 0x8ff, METHOD_BUFFERED, FILE_ANY_ACCESS);

struct DisplayRequest {
    UINT width, height, hz;
    GUID identity;
    char name[14], serial[14];
};
struct DisplayTarget {
    LUID adapter;
    UINT id;
};
struct Watchdog {
    UINT timeout, countdown;
};

static_assert(sizeof(DisplayRequest) == 56);
static_assert(sizeof(DisplayTarget) == 12);

HANDLE open_driver(DWORD &error) {
    HDEVINFO devices = SetupDiGetClassDevsW(&interface_id, nullptr, nullptr,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE);
    if (devices == INVALID_HANDLE_VALUE) {
        error = GetLastError();
        return INVALID_HANDLE_VALUE;
    }
    const auto cleanup = std::unique_ptr<void, decltype(&SetupDiDestroyDeviceInfoList)>(
        devices, SetupDiDestroyDeviceInfoList);
    HANDLE driver = INVALID_HANDLE_VALUE;
    error = ERROR_DEVICE_NOT_CONNECTED;
    for (DWORD index = 0;; ++index) {
        SP_DEVICE_INTERFACE_DATA item{sizeof(item)};
        if (!SetupDiEnumDeviceInterfaces(devices, nullptr, &interface_id, index, &item)) break;
        DWORD size = 0;
        SetupDiGetDeviceInterfaceDetailW(devices, &item, nullptr, 0, &size, nullptr);
        if (size < sizeof(SP_DEVICE_INTERFACE_DETAIL_DATA_W)) continue;
        auto storage = std::make_unique<std::byte[]>(size);
        auto detail = reinterpret_cast<SP_DEVICE_INTERFACE_DETAIL_DATA_W *>(storage.get());
        detail->cbSize = sizeof(*detail);
        if (!SetupDiGetDeviceInterfaceDetailW(devices, &item, detail, size, nullptr, nullptr)) continue;
        driver = CreateFileW(detail->DevicePath, GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (driver != INVALID_HANDLE_VALUE) break;
        error = GetLastError();
    }
    return driver;
}

struct VirtualDisplay {
    HANDLE driver = INVALID_HANDLE_VALUE;
    HANDLE stop = nullptr;
    GUID identity{};
    bool created = false;
    std::thread worker;

    DWORD request(DWORD code, void *data = nullptr, DWORD size = 0,
                  void *response = nullptr, DWORD response_size = 0) {
        DWORD received = 0;
        if (!DeviceIoControl(driver, code, data, size, response, response_size, &received, nullptr)) {
            return GetLastError();
        }
        return received < response_size ? ERROR_INVALID_DATA : ERROR_SUCCESS;
    }

    DWORD start_watchdog() {
        Watchdog watchdog{};
        if (auto result = request(get_watchdog, nullptr, 0, &watchdog, sizeof(watchdog))) return result;
        if (!watchdog.timeout) return ERROR_SUCCESS;
        stop = CreateEventW(nullptr, TRUE, FALSE, nullptr);
        if (!stop) return GetLastError();
        if (auto result = request(ping)) return result;
        worker = std::thread([this, interval = std::clamp(watchdog.timeout, 1u, 60u) * 1000 / 3] {
            while (WaitForSingleObject(stop, interval) == WAIT_TIMEOUT) {
                if (request(ping) != ERROR_SUCCESS) return;
            }
        });
        return ERROR_SUCCESS;
    }

    ~VirtualDisplay() {
        if (stop) SetEvent(stop);
        if (worker.joinable()) worker.join();
        if (created) request(remove_display, &identity, sizeof(identity));
        if (stop) CloseHandle(stop);
        if (driver != INVALID_HANDLE_VALUE) CloseHandle(driver);
    }
};

struct MonitorName {
    const wchar_t *name;
    HMONITOR monitor = nullptr;
};

BOOL CALLBACK match_monitor(HMONITOR monitor, HDC, LPRECT, LPARAM payload) {
    auto &target = *reinterpret_cast<MonitorName *>(payload);
    MONITORINFOEXW info{};
    info.cbSize = sizeof(info);
    if (GetMonitorInfoW(monitor, &info) && std::wcscmp(info.szDevice, target.name) == 0) {
        target.monitor = monitor;
        return FALSE;
    }
    return TRUE;
}

bool same_adapter(LUID a, LUID b) {
    return a.HighPart == b.HighPart && a.LowPart == b.LowPart;
}

HMONITOR monitor_for_target(DisplayTarget target) {
    UINT32 path_count = 0, mode_count = 0;
    if (GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &path_count, &mode_count) != ERROR_SUCCESS) {
        return nullptr;
    }
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(path_count);
    std::vector<DISPLAYCONFIG_MODE_INFO> modes(mode_count);
    if (QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &path_count, paths.data(), &mode_count,
                           modes.data(), nullptr) != ERROR_SUCCESS) {
        return nullptr;
    }
    paths.resize(path_count);
    for (const auto &path : paths) {
        if (path.targetInfo.id != target.id ||
            !same_adapter(path.targetInfo.adapterId, target.adapter)) continue;
        const auto &src = path.sourceInfo;
        if (std::count_if(paths.begin(), paths.end(), [&](const auto &other) {
            return other.sourceInfo.id == src.id && same_adapter(other.sourceInfo.adapterId, src.adapterId);
        }) != 1) return nullptr;
        DISPLAYCONFIG_SOURCE_DEVICE_NAME name{};
        name.header = {DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, sizeof(name), src.adapterId, src.id};
        if (DisplayConfigGetDeviceInfo(&name.header) != ERROR_SUCCESS) return nullptr;
        MonitorName found{name.viewGdiDeviceName};
        EnumDisplayMonitors(nullptr, nullptr, match_monitor, reinterpret_cast<LPARAM>(&found));
        return found.monitor;
    }
    return nullptr;
}
}

extern "C" uint32_t remoters_virtual_display_create(
    uint32_t width, uint32_t height, uint32_t fps, uint32_t *display_id, void **handle) {
    try {
        auto display = std::make_unique<VirtualDisplay>();
        DWORD error = ERROR_SUCCESS;
        display->driver = open_driver(error);
        if (display->driver == INVALID_HANDLE_VALUE) return error;
        uint8_t version[4]{};
        if (auto result = display->request(get_version, nullptr, 0, version, sizeof(version))) return result;
        if (version[0] != 0 || version[1] < 2) return ERROR_REVISION_MISMATCH;
        if (FAILED(CoCreateGuid(&display->identity))) return ERROR_GEN_FAILURE;
        DisplayRequest spec{width, height, fps, display->identity, "Remoters", {}};
        std::snprintf(spec.serial, sizeof(spec.serial), "%08X", display->identity.Data1);
        DisplayTarget target{};
        if (auto result = display->request(add_display, &spec, sizeof(spec), &target, sizeof(target))) {
            return result;
        }
        display->created = true;
        if (auto result = display->start_watchdog()) return result;
        for (int tries = 0; tries < 30; ++tries) {
            if (HMONITOR monitor = monitor_for_target(target)) {
                *display_id = static_cast<uint32_t>(reinterpret_cast<uintptr_t>(monitor));
                *handle = display.release();
                return ERROR_SUCCESS;
            }
            Sleep(50);
        }
        return ERROR_TIMEOUT;
    } catch (const std::bad_alloc &) {
        return ERROR_OUTOFMEMORY;
    } catch (const std::system_error &) {
        return ERROR_GEN_FAILURE;
    }
}

extern "C" void remoters_virtual_display_destroy(void *display) {
    delete static_cast<VirtualDisplay *>(display);
}

extern "C" bool remoters_move_pointer(int32_t x, int32_t y) {
    const int left = GetSystemMetrics(SM_XVIRTUALSCREEN);
    const int top = GetSystemMetrics(SM_YVIRTUALSCREEN);
    const int width = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    const int height = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    if (width <= 0 || height <= 0) return false;
    // Aim at the center of the pixel represented by the 0..65535 interval.
    const auto normalize = [](int point, int origin, int size) {
        const auto offset = std::clamp<int64_t>(static_cast<int64_t>(point) - origin, 0, size - 1);
        return static_cast<LONG>((offset * 65536 + 32768) / size);
    };
    INPUT input{};
    input.type = INPUT_MOUSE;
    input.mi.dx = normalize(x, left, width);
    input.mi.dy = normalize(y, top, height);
    input.mi.dwFlags = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK;
    return SendInput(1, &input, sizeof(input)) == 1;
}
