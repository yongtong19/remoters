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

DWORD device_request(HANDLE driver, DWORD code, void *data = nullptr, DWORD size = 0,
                     void *response = nullptr, DWORD response_size = 0) {
    DWORD received = 0;
    if (!DeviceIoControl(driver, code, data, size, response, response_size, &received, nullptr)) {
        return GetLastError();
    }
    return received < response_size ? ERROR_INVALID_DATA : ERROR_SUCCESS;
}

DWORD check_driver_version(HANDLE driver) {
    uint8_t version[4]{};
    if (auto result = device_request(driver, get_version, nullptr, 0, version, sizeof(version))) {
        return result;
    }
    if (version[0] != 0 || version[1] < 2) return ERROR_REVISION_MISMATCH;
    return ERROR_SUCCESS;
}

struct VirtualDisplay {
    HANDLE driver = INVALID_HANDLE_VALUE;
    HANDLE stop = nullptr;
    GUID identity{};
    bool created = false;
    bool topology_changed = false;
    std::vector<DISPLAYCONFIG_PATH_INFO> saved_paths;
    std::vector<DISPLAYCONFIG_MODE_INFO> saved_modes;
    std::thread worker;

    DWORD request(DWORD code, void *data = nullptr, DWORD size = 0,
                  void *response = nullptr, DWORD response_size = 0) {
        return device_request(driver, code, data, size, response, response_size);
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
        if (topology_changed && !saved_paths.empty()) {
            const DWORD restored = SetDisplayConfig(
                static_cast<UINT32>(saved_paths.size()),
                saved_paths.data(),
                static_cast<UINT32>(saved_modes.size()),
                saved_modes.data(),
                SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES);
            if (created) request(remove_display, &identity, sizeof(identity));
            if (restored != ERROR_SUCCESS) {
                Sleep(100);
                if (SetDisplayConfig(
                        static_cast<UINT32>(saved_paths.size()),
                        saved_paths.data(),
                        static_cast<UINT32>(saved_modes.size()),
                        saved_modes.data(),
                        SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES) != ERROR_SUCCESS) {
                    SetDisplayConfig(0, nullptr, 0, nullptr, SDC_APPLY | SDC_USE_DATABASE_CURRENT);
                }
            }
        } else if (created) {
            request(remove_display, &identity, sizeof(identity));
        }
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

bool query_active_display_config(
    std::vector<DISPLAYCONFIG_PATH_INFO> &paths,
    std::vector<DISPLAYCONFIG_MODE_INFO> &modes) {
    UINT32 path_count = 0, mode_count = 0;
    if (GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &path_count, &mode_count) != ERROR_SUCCESS) {
        return false;
    }
    paths.resize(path_count);
    modes.resize(mode_count);
    if (QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &path_count, paths.data(), &mode_count,
                           modes.data(), nullptr) != ERROR_SUCCESS) {
        paths.clear();
        modes.clear();
        return false;
    }
    paths.resize(path_count);
    modes.resize(mode_count);
    return !paths.empty();
}

bool is_primary_monitor(HMONITOR monitor) {
    MONITORINFO info{};
    info.cbSize = sizeof(info);
    return GetMonitorInfoW(monitor, &info) && (info.dwFlags & MONITORINFOF_PRIMARY) != 0;
}

class ScopedPerMonitorDpiV2 {
public:
    ScopedPerMonitorDpiV2() {
        using SetDpiCtxFn = DPI_AWARENESS_CONTEXT(WINAPI *)(DPI_AWARENESS_CONTEXT);
        set_dpi_ctx_ = reinterpret_cast<SetDpiCtxFn>(
            GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetThreadDpiAwarenessContext"));
        if (set_dpi_ctx_) {
            prev_dpi_ = set_dpi_ctx_(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    ~ScopedPerMonitorDpiV2() {
        if (set_dpi_ctx_ && prev_dpi_) {
            set_dpi_ctx_(prev_dpi_);
        }
    }

    ScopedPerMonitorDpiV2(const ScopedPerMonitorDpiV2 &) = delete;
    ScopedPerMonitorDpiV2 &operator=(const ScopedPerMonitorDpiV2 &) = delete;

private:
    using SetDpiCtxFn = DPI_AWARENESS_CONTEXT(WINAPI *)(DPI_AWARENESS_CONTEXT);
    SetDpiCtxFn set_dpi_ctx_ = nullptr;
    DPI_AWARENESS_CONTEXT prev_dpi_ = nullptr;
};

bool query_display_config_flags(
    UINT32 flags,
    std::vector<DISPLAYCONFIG_PATH_INFO> &paths,
    std::vector<DISPLAYCONFIG_MODE_INFO> &modes) {
    UINT32 path_count = 0, mode_count = 0;
    if (GetDisplayConfigBufferSizes(flags, &path_count, &mode_count) != ERROR_SUCCESS) {
        return false;
    }
    paths.resize(path_count);
    modes.resize(mode_count);
    if (QueryDisplayConfig(flags, &path_count, paths.data(), &mode_count,
                           modes.data(), nullptr) != ERROR_SUCCESS) {
        paths.clear();
        modes.clear();
        return false;
    }
    paths.resize(path_count);
    modes.resize(mode_count);
    return !paths.empty();
}

bool get_target_device_name(DisplayTarget target, wchar_t out_name[CCHDEVICENAME], bool allow_cloned = false) {
    std::vector<DISPLAYCONFIG_PATH_INFO> paths;
    std::vector<DISPLAYCONFIG_MODE_INFO> modes;
    if (!query_active_display_config(paths, modes)) {
        return false;
    }
    for (const auto &path : paths) {
        if (path.targetInfo.id != target.id ||
            !same_adapter(path.targetInfo.adapterId, target.adapter)) continue;
        const auto &src = path.sourceInfo;
        if (!allow_cloned && std::count_if(paths.begin(), paths.end(), [&](const auto &other) {
            return other.sourceInfo.id == src.id && same_adapter(other.sourceInfo.adapterId, src.adapterId);
        }) != 1) return false;
        DISPLAYCONFIG_SOURCE_DEVICE_NAME name{};
        name.header = {DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, sizeof(name), src.adapterId, src.id};
        if (DisplayConfigGetDeviceInfo(&name.header) != ERROR_SUCCESS) return false;
        std::wcsncpy(out_name, name.viewGdiDeviceName, CCHDEVICENAME - 1);
        out_name[CCHDEVICENAME - 1] = L'\0';
        return out_name[0] != L'\0';
    }
    return false;
}

bool target_present_in_active_config(DisplayTarget target) {
    std::vector<DISPLAYCONFIG_PATH_INFO> paths;
    std::vector<DISPLAYCONFIG_MODE_INFO> modes;
    if (!query_active_display_config(paths, modes)) {
        return false;
    }
    return std::any_of(paths.begin(), paths.end(), [&](const auto &path) {
        return path.targetInfo.id == target.id &&
               same_adapter(path.targetInfo.adapterId, target.adapter);
    });
}

bool activate_virtual_display(DisplayTarget target, uint32_t width, uint32_t height) {
    std::vector<DISPLAYCONFIG_PATH_INFO> paths;
    std::vector<DISPLAYCONFIG_MODE_INFO> modes;
    if (!query_active_display_config(paths, modes)) {
        // Fallback: if Windows left the newly attached SudoVDA target inactive, query all paths.
        if (!query_display_config_flags(QDC_ALL_PATHS, paths, modes)) {
            return false;
        }
    }
    const auto it = std::find_if(paths.begin(), paths.end(), [&](const auto &path) {
        return path.targetInfo.id == target.id &&
               same_adapter(path.targetInfo.adapterId, target.adapter);
    });
    if (it == paths.end()) {
        return false;
    }

    const bool has_multiple_active = paths.size() > 1;
    const bool source_cloned = has_multiple_active &&
        std::count_if(paths.begin(), paths.end(), [&](const auto &other) {
            return other.sourceInfo.id == it->sourceInfo.id &&
                   same_adapter(other.sourceInfo.adapterId, it->sourceInfo.adapterId);
        }) > 1;
    const bool already_exclusive_active =
        paths.size() == 1 && (it->flags & DISPLAYCONFIG_PATH_ACTIVE) != 0;
    if (already_exclusive_active && !source_cloned) {
        return false;
    }

    // Strategy 1: make the virtual display the sole active display at (0, 0) with the requested
    // source resolution so Windows neither clones a 4K physical monitor nor leaves desktop UI off-screen.
    DISPLAYCONFIG_PATH_INFO exclusive_path = *it;
    exclusive_path.flags |= DISPLAYCONFIG_PATH_ACTIVE;
    std::vector<DISPLAYCONFIG_MODE_INFO> exclusive_modes;
    if (it->sourceInfo.modeInfoIdx != DISPLAYCONFIG_PATH_MODE_IDX_INVALID &&
        it->sourceInfo.modeInfoIdx < modes.size()) {
        DISPLAYCONFIG_MODE_INFO src_mode = modes[it->sourceInfo.modeInfoIdx];
        src_mode.sourceMode.position = {0, 0};
        if (width >= 2 && height >= 2) {
            src_mode.sourceMode.width = width;
            src_mode.sourceMode.height = height;
        }
        exclusive_modes.push_back(src_mode);
        exclusive_path.sourceInfo.modeInfoIdx = 0;
    } else {
        exclusive_path.sourceInfo.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    }
    if (it->targetInfo.modeInfoIdx != DISPLAYCONFIG_PATH_MODE_IDX_INVALID &&
        it->targetInfo.modeInfoIdx < modes.size()) {
        DISPLAYCONFIG_MODE_INFO tgt_mode = modes[it->targetInfo.modeInfoIdx];
        // Clear target mode when source resolution differs from the existing target active size
        // so Windows CCD picks the exact custom mode matching `width x height` instead of scaling.
        const auto &active_size = tgt_mode.targetMode.targetVideoSignalInfo.activeSize;
        if (width >= 2 && height >= 2 &&
            (active_size.cx != width || active_size.cy != height)) {
            exclusive_path.targetInfo.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
        } else {
            exclusive_modes.push_back(tgt_mode);
            exclusive_path.targetInfo.modeInfoIdx =
                static_cast<UINT32>(exclusive_modes.size() - 1);
        }
    } else {
        exclusive_path.targetInfo.modeInfoIdx = DISPLAYCONFIG_PATH_MODE_IDX_INVALID;
    }

    if (SetDisplayConfig(
            1,
            &exclusive_path,
            static_cast<UINT32>(exclusive_modes.size()),
            exclusive_modes.empty() ? nullptr : exclusive_modes.data(),
            SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES) == ERROR_SUCCESS) {
        return true;
    }

    // Strategy 2 fallback: keep other monitors but move the virtual display to (0, 0) as primary.
    if (it->sourceInfo.modeInfoIdx == DISPLAYCONFIG_PATH_MODE_IDX_INVALID ||
        it->sourceInfo.modeInfoIdx >= modes.size()) {
        return false;
    }
    const POINTL origin = modes[it->sourceInfo.modeInfoIdx].sourceMode.position;
    if (origin.x != 0 || origin.y != 0) {
        for (auto &mode : modes) {
            if (mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE) {
                mode.sourceMode.position.x -= origin.x;
                mode.sourceMode.position.y -= origin.y;
            }
        }
    }
    return SetDisplayConfig(
               static_cast<UINT32>(paths.size()),
               paths.data(),
               static_cast<UINT32>(modes.size()),
               modes.data(),
               SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES) == ERROR_SUCCESS;
}

bool apply_virtual_display_mode(DisplayTarget target, uint32_t width, uint32_t height, uint32_t fps) {
    if (width < 2 || height < 2) {
        return false;
    }
    wchar_t device_name[CCHDEVICENAME]{};
    if (!get_target_device_name(target, device_name)) {
        return false;
    }

    DEVMODEW current{};
    current.dmSize = sizeof(current);
    if (EnumDisplaySettingsW(device_name, ENUM_CURRENT_SETTINGS, &current)) {
        const bool size_matches = current.dmPelsWidth == width && current.dmPelsHeight == height;
        const bool fps_matches = fps == 0 || current.dmDisplayFrequency == fps;
        if (size_matches && fps_matches) {
            return true;
        }
    }

    // Search the raw mode list exposed by SudoVDA (including custom non-16:9 modes like
    // 2400x1080, 2640x1200, 2772x1240) for an exact width x height match.
    bool found_best = false;
    DEVMODEW best{};
    uint32_t best_fps_diff = UINT32_MAX;
    for (DWORD mode_idx = 0;; ++mode_idx) {
        DEVMODEW candidate{};
        candidate.dmSize = sizeof(candidate);
        if (!EnumDisplaySettingsExW(device_name, mode_idx, &candidate, EDS_RAWMODE)) {
            break;
        }
        if (candidate.dmPelsWidth != width || candidate.dmPelsHeight != height) {
            continue;
        }
        if (candidate.dmBitsPerPel != 0 && candidate.dmBitsPerPel < 32) {
            continue;
        }
        const uint32_t cand_fps = candidate.dmDisplayFrequency;
        const uint32_t diff =
            fps == 0 ? 0 : (cand_fps >= fps ? cand_fps - fps : fps - cand_fps);
        if (!found_best || diff < best_fps_diff) {
            best = candidate;
            best_fps_diff = diff;
            found_best = true;
            if (diff == 0) {
                break;
            }
        }
    }

    auto try_change = [&](DEVMODEW &mode) -> bool {
        const LONG res = ChangeDisplaySettingsExW(device_name, &mode, nullptr, 0, nullptr);
        return res == DISP_CHANGE_SUCCESSFUL;
    };

    bool switched = false;
    if (found_best) {
        best.dmFields |= DM_PELSWIDTH | DM_PELSHEIGHT;
        if (best.dmDisplayFrequency > 0) {
            best.dmFields |= DM_DISPLAYFREQUENCY;
        }
        if (best.dmBitsPerPel > 0) {
            best.dmFields |= DM_BITSPERPEL;
        }
        switched = try_change(best);
    }

    if (!switched) {
        DEVMODEW direct{};
        direct.dmSize = sizeof(direct);
        direct.dmPelsWidth = width;
        direct.dmPelsHeight = height;
        direct.dmBitsPerPel = 32;
        direct.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT | DM_BITSPERPEL;
        if (fps > 0) {
            direct.dmDisplayFrequency = fps;
            direct.dmFields |= DM_DISPLAYFREQUENCY;
            switched = try_change(direct);
        }
        if (!switched) {
            direct.dmFields = DM_PELSWIDTH | DM_PELSHEIGHT;
            direct.dmDisplayFrequency = 0;
            switched = try_change(direct);
        }
    }

    if (!switched) {
        return false;
    }

    for (int tries = 0; tries < 20; ++tries) {
        DEVMODEW verify{};
        verify.dmSize = sizeof(verify);
        if (EnumDisplaySettingsW(device_name, ENUM_CURRENT_SETTINGS, &verify) &&
            verify.dmPelsWidth == width &&
            verify.dmPelsHeight == height) {
            return true;
        }
        Sleep(50);
    }
    return false;
}

HMONITOR monitor_for_target(DisplayTarget target) {
    wchar_t device_name[CCHDEVICENAME]{};
    if (!get_target_device_name(target, device_name)) {
        return nullptr;
    }
    MonitorName found{device_name};
    EnumDisplayMonitors(nullptr, nullptr, match_monitor, reinterpret_cast<LPARAM>(&found));
    return found.monitor;
}
}

extern "C" uint32_t remoters_virtual_display_available() {
    DWORD error = ERROR_SUCCESS;
    HANDLE driver = open_driver(error);
    if (driver == INVALID_HANDLE_VALUE) return error;
    const DWORD result = check_driver_version(driver);
    CloseHandle(driver);
    return result;
}

extern "C" uint32_t remoters_virtual_display_create(
    uint32_t width, uint32_t height, uint32_t fps, uint32_t *display_id, void **handle) {
    if (!display_id || !handle) return ERROR_INVALID_PARAMETER;
    ScopedPerMonitorDpiV2 dpi_guard;
    try {
        auto display = std::make_unique<VirtualDisplay>();
        DWORD error = ERROR_SUCCESS;
        display->driver = open_driver(error);
        if (display->driver == INVALID_HANDLE_VALUE) return error;
        if (auto result = check_driver_version(display->driver)) return result;
        if (FAILED(CoCreateGuid(&display->identity))) return ERROR_GEN_FAILURE;
        query_active_display_config(display->saved_paths, display->saved_modes);
        DisplayRequest spec{width, height, fps, display->identity, "Remoters", {}};
        std::snprintf(spec.serial, sizeof(spec.serial), "%08X", display->identity.Data1);
        DisplayTarget target{};
        if (auto result = display->request(add_display, &spec, sizeof(spec), &target, sizeof(target))) {
            return result;
        }
        display->created = true;
        if (auto result = display->start_watchdog()) return result;
        HMONITOR monitor = nullptr;
        bool present = false;
        for (int tries = 0; tries < 50; ++tries) {
            monitor = monitor_for_target(target);
            if (monitor) {
                present = true;
                break;
            }
            // If Windows initially attached the SudoVDA display in Clone/Duplicate mode,
            // `monitor_for_target` waits for an uncloned source, so break once present in CCD
            // after a short grace window and let `activate_virtual_display` make it exclusive.
            if (tries >= 10 && target_present_in_active_config(target)) {
                present = true;
                break;
            }
            Sleep(50);
        }
        if (!present) return ERROR_TIMEOUT;
        display->topology_changed = activate_virtual_display(target, width, height);
        if (display->topology_changed || !monitor) {
            HMONITOR switched = monitor;
            for (int tries = 0; tries < 40; ++tries) {
                if (HMONITOR candidate = monitor_for_target(target)) {
                    switched = candidate;
                    if (!display->topology_changed || is_primary_monitor(candidate)) break;
                }
                Sleep(50);
            }
            monitor = switched;
        }
        if (!monitor) return ERROR_TIMEOUT;
        // Explicitly enforce the requested width x height @ fps (including custom non-16:9
        // resolutions such as 2400x1080, 2640x1200, 2772x1240) in case Windows CCD kept a
        // 16:9 or 4K fallback mode during topology activation.
        apply_virtual_display_mode(target, width, height, fps);
        if (HMONITOR settled = monitor_for_target(target)) {
            monitor = settled;
        }
        *display_id = static_cast<uint32_t>(reinterpret_cast<uintptr_t>(monitor));
        *handle = display.release();
        return ERROR_SUCCESS;
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
    ScopedPerMonitorDpiV2 dpi_guard;
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

extern "C" void remoters_nudge_monitor(int32_t x, int32_t y) {
    // Trigger one DWM composition pass on a freshly created idle monitor so
    // DXGI Desktop Duplication receives its initial desktop frame immediately.
    using SetDpiCtxFn = DPI_AWARENESS_CONTEXT(WINAPI *)(DPI_AWARENESS_CONTEXT);
    auto set_dpi_ctx = reinterpret_cast<SetDpiCtxFn>(
        GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetThreadDpiAwarenessContext"));
    DPI_AWARENESS_CONTEXT prev_dpi =
        set_dpi_ctx ? set_dpi_ctx(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) : nullptr;

    HMODULE gdi32 = LoadLibraryW(L"gdi32.dll");
    HMODULE dwmapi = LoadLibraryW(L"dwmapi.dll");
    using CreateCompatibleDCFn = HDC(WINAPI *)(HDC);
    using DeleteDCFn = BOOL(WINAPI *)(HDC);
    using SelectObjectFn = HGDIOBJ(WINAPI *)(HDC, HGDIOBJ);
    using DeleteObjectFn = BOOL(WINAPI *)(HGDIOBJ);
    using CreateDIBSectionFn = HBITMAP(WINAPI *)(HDC, const BITMAPINFO *, UINT, void **, HANDLE, DWORD);
    using DwmFlushFn = HRESULT(WINAPI *)();
    using DwmSetAttrFn = HRESULT(WINAPI *)(HWND, DWORD, LPCVOID, DWORD);

    auto create_dc = gdi32 ? reinterpret_cast<CreateCompatibleDCFn>(GetProcAddress(gdi32, "CreateCompatibleDC")) : nullptr;
    auto delete_dc = gdi32 ? reinterpret_cast<DeleteDCFn>(GetProcAddress(gdi32, "DeleteDC")) : nullptr;
    auto select_obj = gdi32 ? reinterpret_cast<SelectObjectFn>(GetProcAddress(gdi32, "SelectObject")) : nullptr;
    auto delete_obj = gdi32 ? reinterpret_cast<DeleteObjectFn>(GetProcAddress(gdi32, "DeleteObject")) : nullptr;
    auto create_dib = gdi32 ? reinterpret_cast<CreateDIBSectionFn>(GetProcAddress(gdi32, "CreateDIBSection")) : nullptr;
    auto dwm_flush = dwmapi ? reinterpret_cast<DwmFlushFn>(GetProcAddress(dwmapi, "DwmFlush")) : nullptr;
    auto dwm_set_attr = dwmapi ? reinterpret_cast<DwmSetAttrFn>(GetProcAddress(dwmapi, "DwmSetWindowAttribute")) : nullptr;

    constexpr int kSize = 4;
    HWND hwnd = CreateWindowExW(
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
        L"STATIC",
        nullptr,
        WS_POPUP,
        x + 8,
        y + 8,
        kSize,
        kSize,
        nullptr,
        nullptr,
        GetModuleHandleW(nullptr),
        nullptr);
    if (hwnd) {
        if (dwm_set_attr) {
            constexpr DWORD kDwmwaTransitionsForcedisabled = 3;
            BOOL disable = TRUE;
            dwm_set_attr(hwnd, kDwmwaTransitionsForcedisabled, &disable, sizeof(disable));
        }
        HDC screen_dc = GetDC(nullptr);
        HDC mem_dc = (screen_dc && create_dc) ? create_dc(screen_dc) : nullptr;
        void *bits = nullptr;
        BITMAPINFO bmi{};
        bmi.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
        bmi.bmiHeader.biWidth = kSize;
        bmi.bmiHeader.biHeight = -kSize;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;
        HBITMAP dib = (mem_dc && create_dib)
            ? create_dib(mem_dc, &bmi, DIB_RGB_COLORS, &bits, nullptr, 0)
            : nullptr;
        HGDIOBJ prev_obj = (mem_dc && dib && select_obj) ? select_obj(mem_dc, dib) : nullptr;

        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        if (dib && bits) {
            auto *pixels = static_cast<uint32_t *>(bits);
            POINT dst_pt{x + 8, y + 8};
            POINT src_pt{0, 0};
            SIZE sz{kSize, kSize};
            BLENDFUNCTION blend{AC_SRC_OVER, 0, 255, AC_SRC_ALPHA};
            for (uint32_t color : {0xFF010101u, 0xFF020202u}) {
                std::fill_n(pixels, kSize * kSize, color);
                UpdateLayeredWindow(hwnd, screen_dc, &dst_pt, &sz, mem_dc, &src_pt, 0, &blend, ULW_ALPHA);
                if (dwm_flush) {
                    dwm_flush();
                } else {
                    Sleep(20);
                }
            }
        }

        MSG msg{};
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessage(&msg);
        }
        DestroyWindow(hwnd);
        if (dwm_flush) {
            dwm_flush();
        }
        while (PeekMessageW(&msg, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&msg);
            DispatchMessage(&msg);
        }
        if (mem_dc && prev_obj && select_obj) {
            select_obj(mem_dc, prev_obj);
        }
        if (dib && delete_obj) {
            delete_obj(dib);
        }
        if (mem_dc && delete_dc) {
            delete_dc(mem_dc);
        }
        if (screen_dc) {
            ReleaseDC(nullptr, screen_dc);
        }
    }
    if (dwmapi) {
        FreeLibrary(dwmapi);
    }
    if (gdi32) {
        FreeLibrary(gdi32);
    }
    if (set_dpi_ctx && prev_dpi) {
        set_dpi_ctx(prev_dpi);
    }
}

extern "C" void remoters_nudge_pointer(int32_t x, int32_t y) {
    // Move the pointer onto an idle monitor (such as a newly attached SudoVDA display)
    // and back so DXGI Desktop Duplication emits an initial frame immediately.
    using SetDpiCtxFn = DPI_AWARENESS_CONTEXT(WINAPI *)(DPI_AWARENESS_CONTEXT);
    auto set_dpi_ctx = reinterpret_cast<SetDpiCtxFn>(
        GetProcAddress(GetModuleHandleW(L"user32.dll"), "SetThreadDpiAwarenessContext"));
    DPI_AWARENESS_CONTEXT prev_dpi =
        set_dpi_ctx ? set_dpi_ctx(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) : nullptr;

    POINT saved{};
    const BOOL has_saved = GetCursorPos(&saved);
    remoters_move_pointer(x, y);
    Sleep(16);
    remoters_move_pointer(x + 1, y);
    Sleep(35);
    if (has_saved) {
        remoters_move_pointer(saved.x, saved.y);
    }

    if (set_dpi_ctx && prev_dpi) {
        set_dpi_ctx(prev_dpi);
    }
}
