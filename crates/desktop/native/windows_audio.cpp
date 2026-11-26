#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <mmreg.h>
#include <objbase.h>

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <mutex>
#include <new>
#include <string>
#include <thread>
#include <vector>

namespace {

constexpr uint32_t kTargetSampleRate = 48000;
constexpr uint16_t kTargetChannels = 2;
constexpr size_t kChunkFrames = 960;
constexpr size_t kChunkSamples = kChunkFrames * kTargetChannels;
constexpr size_t kSilentTailChunks = 5;
constexpr size_t kRingCapacity = 24000; // 250 ms at 48 kHz stereo
constexpr size_t kTargetDriftSamples = 3840; // 40 ms
constexpr size_t kMaxDriftSamples = 9600;    // 100 ms

#ifndef AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
#define AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM 0x80000000
#endif
#ifndef AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY
#define AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY 0x08000000
#endif

const GUID kSubtypePcm{
    0x00000001, 0x0000, 0x0010, {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};
const GUID kSubtypeIeeeFloat{
    0x00000003, 0x0000, 0x0010, {0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71}};

template <typename T>
class ComPtr {
public:
    ComPtr() = default;
    ~ComPtr() { reset(); }
    ComPtr(const ComPtr &) = delete;
    ComPtr &operator=(const ComPtr &) = delete;

    T **put() {
        reset();
        return &ptr_;
    }

    T *get() const { return ptr_; }
    T *operator->() const { return ptr_; }
    explicit operator bool() const { return ptr_ != nullptr; }

    void reset() {
        if (ptr_) {
            ptr_->Release();
            ptr_ = nullptr;
        }
    }

private:
    T *ptr_ = nullptr;
};

struct CoInitGuard {
    bool initialized = false;
    CoInitGuard() {
        const HRESULT hr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        initialized = SUCCEEDED(hr);
    }
    ~CoInitGuard() {
        if (initialized) {
            CoUninitialize();
        }
    }
};

struct CoTaskMemGuard {
    WAVEFORMATEX *ptr = nullptr;
    ~CoTaskMemGuard() {
        if (ptr) {
            CoTaskMemFree(ptr);
        }
    }
};

struct FormatInfo {
    uint32_t sample_rate = kTargetSampleRate;
    uint16_t channels = kTargetChannels;
    bool is_float = false;
    bool is_int16 = false;
};

bool parse_wave_format(const WAVEFORMATEX *wf, FormatInfo &out) {
    if (!wf || wf->nChannels == 0 || wf->nSamplesPerSec == 0) {
        return false;
    }
    out.sample_rate = wf->nSamplesPerSec;
    out.channels = wf->nChannels;
    out.is_float = false;
    out.is_int16 = false;

    if (wf->wFormatTag == WAVE_FORMAT_PCM && wf->wBitsPerSample == 16) {
        out.is_int16 = true;
        return true;
    }
    if (wf->wFormatTag == WAVE_FORMAT_IEEE_FLOAT && wf->wBitsPerSample == 32) {
        out.is_float = true;
        return true;
    }
    if (wf->wFormatTag == WAVE_FORMAT_EXTENSIBLE &&
        wf->cbSize >= sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)) {
        const auto *ext = reinterpret_cast<const WAVEFORMATEXTENSIBLE *>(wf);
        if (IsEqualGUID(ext->SubFormat, kSubtypePcm) && wf->wBitsPerSample == 16) {
            out.is_int16 = true;
            return true;
        }
        if (IsEqualGUID(ext->SubFormat, kSubtypeIeeeFloat) && wf->wBitsPerSample == 32) {
            out.is_float = true;
            return true;
        }
    }
    return false;
}

WAVEFORMATEX make_target_format() {
    WAVEFORMATEX wf{};
    wf.wFormatTag = WAVE_FORMAT_PCM;
    wf.nChannels = kTargetChannels;
    wf.nSamplesPerSec = kTargetSampleRate;
    wf.wBitsPerSample = 16;
    wf.nBlockAlign = kTargetChannels * sizeof(int16_t);
    wf.nAvgBytesPerSec = kTargetSampleRate * wf.nBlockAlign;
    wf.cbSize = 0;
    return wf;
}

inline int16_t float_to_i16(float v) {
    if (std::isnan(v)) {
        return 0;
    }
    if (v >= 1.0f) {
        return 32767;
    }
    if (v <= -1.0f) {
        return -32768;
    }
    const long rounded = std::lround(v * 32767.0f);
    return static_cast<int16_t>(std::clamp<long>(rounded, -32768, 32767));
}

inline float read_frame_channel(const uint8_t *data, const FormatInfo &fmt, size_t frame, uint16_t ch) {
    const uint16_t c = ch < fmt.channels ? ch : 0;
    const size_t idx = frame * fmt.channels + c;
    if (fmt.is_float) {
        return reinterpret_cast<const float *>(data)[idx];
    }
    return static_cast<float>(reinterpret_cast<const int16_t *>(data)[idx]) / 32768.0f;
}

void convert_to_target_stereo_i16(
    const uint8_t *data,
    size_t in_frames,
    const FormatInfo &fmt,
    std::vector<int16_t> &accum
) {
    if (in_frames == 0 || !data) {
        return;
    }
    if (fmt.sample_rate == kTargetSampleRate && fmt.channels == kTargetChannels && fmt.is_int16) {
        const auto *src = reinterpret_cast<const int16_t *>(data);
        accum.insert(accum.end(), src, src + in_frames * kTargetChannels);
        return;
    }

    const uint16_t right_ch = fmt.channels >= 2 ? 1 : 0;
    if (fmt.sample_rate == kTargetSampleRate) {
        const size_t base = accum.size();
        accum.resize(base + in_frames * kTargetChannels);
        for (size_t f = 0; f < in_frames; ++f) {
            accum[base + f * 2] = float_to_i16(read_frame_channel(data, fmt, f, 0));
            accum[base + f * 2 + 1] = float_to_i16(read_frame_channel(data, fmt, f, right_ch));
        }
        return;
    }

    const long long scaled = std::llround(
        static_cast<double>(in_frames) * static_cast<double>(kTargetSampleRate) /
        static_cast<double>(fmt.sample_rate));
    const size_t out_frames = scaled > 0 ? static_cast<size_t>(scaled) : 1;
    const size_t base = accum.size();
    accum.resize(base + out_frames * kTargetChannels);
    const double ratio = static_cast<double>(fmt.sample_rate) / static_cast<double>(kTargetSampleRate);
    for (size_t f = 0; f < out_frames; ++f) {
        const double pos = static_cast<double>(f) * ratio;
        size_t idx0 = static_cast<size_t>(pos);
        if (idx0 >= in_frames) {
            idx0 = in_frames - 1;
        }
        const size_t idx1 = (idx0 + 1 < in_frames) ? (idx0 + 1) : idx0;
        const float frac = static_cast<float>(pos - static_cast<double>(idx0));

        const float l0 = read_frame_channel(data, fmt, idx0, 0);
        const float l1 = read_frame_channel(data, fmt, idx1, 0);
        const float r0 = read_frame_channel(data, fmt, idx0, right_ch);
        const float r1 = read_frame_channel(data, fmt, idx1, right_ch);

        accum[base + f * 2] = float_to_i16(l0 + (l1 - l0) * frac);
        accum[base + f * 2 + 1] = float_to_i16(r0 + (r1 - r0) * frac);
    }
}

bool is_all_zero(const int16_t *samples, size_t count) {
    for (size_t i = 0; i < count; ++i) {
        if (samples[i] != 0) {
            return false;
        }
    }
    return true;
}

using AudioCaptureCallback = void (*)(void *ctx, const int16_t *samples, size_t sample_count);

struct CaptureState {
    AudioCaptureCallback callback = nullptr;
    void *ctx = nullptr;
    std::atomic<bool> stop{false};
    std::thread worker;
};

void sleep_interruptible(const std::atomic<bool> &stop, uint32_t total_ms) {
    for (uint32_t elapsed = 0; elapsed < total_ms && !stop.load(std::memory_order_acquire);
         elapsed += 50) {
        Sleep(50);
    }
}

bool get_device_id(IMMDevice *device, std::wstring &out) {
    out.clear();
    if (!device) {
        return false;
    }
    LPWSTR raw = nullptr;
    if (FAILED(device->GetId(&raw)) || !raw) {
        return false;
    }
    out.assign(raw);
    CoTaskMemFree(raw);
    return true;
}

bool default_render_device_changed(IMMDeviceEnumerator *enumerator, const std::wstring &active_id) {
    if (!enumerator || active_id.empty()) {
        return false;
    }
    ComPtr<IMMDevice> current;
    if (FAILED(enumerator->GetDefaultAudioEndpoint(eRender, eConsole, current.put()))) {
        return true;
    }
    std::wstring current_id;
    if (!get_device_id(current.get(), current_id)) {
        return true;
    }
    return current_id != active_id;
}

void capture_thread_main(CaptureState *state) {
    CoInitGuard co_init;
    if (!co_init.initialized) {
        return;
    }

    std::vector<int16_t> accum;
    accum.reserve(kChunkSamples * 4);
    size_t silent_tail = 0;

    while (!state->stop.load(std::memory_order_acquire)) {
        ComPtr<IMMDeviceEnumerator> enumerator;
        HRESULT hr = CoCreateInstance(
            __uuidof(MMDeviceEnumerator),
            nullptr,
            CLSCTX_ALL,
            __uuidof(IMMDeviceEnumerator),
            reinterpret_cast<void **>(enumerator.put()));
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        ComPtr<IMMDevice> device;
        hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, device.put());
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }
        std::wstring active_device_id;
        get_device_id(device.get(), active_device_id);

        ComPtr<IAudioClient> client;
        hr = device->Activate(
            __uuidof(IAudioClient),
            CLSCTX_ALL,
            nullptr,
            reinterpret_cast<void **>(client.put()));
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        FormatInfo fmt{};
        WAVEFORMATEX target = make_target_format();
        hr = client->Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM |
                AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            1000000, // 100 ms
            0,
            &target,
            nullptr);
        if (SUCCEEDED(hr)) {
            fmt.sample_rate = kTargetSampleRate;
            fmt.channels = kTargetChannels;
            fmt.is_int16 = true;
            fmt.is_float = false;
        } else {
            // Re-activate a fresh IAudioClient because a failed Initialize can invalidate it.
            hr = device->Activate(
                __uuidof(IAudioClient),
                CLSCTX_ALL,
                nullptr,
                reinterpret_cast<void **>(client.put()));
            if (FAILED(hr)) {
                sleep_interruptible(state->stop, 1000);
                continue;
            }
            CoTaskMemGuard mix_guard;
            hr = client->GetMixFormat(&mix_guard.ptr);
            if (FAILED(hr) || !parse_wave_format(mix_guard.ptr, fmt)) {
                sleep_interruptible(state->stop, 1000);
                continue;
            }
            hr = client->Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                1000000,
                0,
                mix_guard.ptr,
                nullptr);
            if (FAILED(hr)) {
                sleep_interruptible(state->stop, 1000);
                continue;
            }
        }

        ComPtr<IAudioCaptureClient> capture_client;
        hr = client->GetService(
            __uuidof(IAudioCaptureClient),
            reinterpret_cast<void **>(capture_client.put()));
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        hr = client->Start();
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        accum.clear();
        silent_tail = 0;
        bool device_switched = false;
        ULONGLONG last_default_check = GetTickCount64();

        while (!state->stop.load(std::memory_order_acquire)) {
            const ULONGLONG now = GetTickCount64();
            if (now - last_default_check >= 500) {
                last_default_check = now;
                if (default_render_device_changed(enumerator.get(), active_device_id)) {
                    device_switched = true;
                    break;
                }
            }
            UINT32 packet_frames = 0;
            hr = capture_client->GetNextPacketSize(&packet_frames);
            if (hr == AUDCLNT_E_DEVICE_INVALIDATED || hr == AUDCLNT_E_SERVICE_NOT_RUNNING ||
                FAILED(hr)) {
                break;
            }
            if (packet_frames == 0) {
                Sleep(6);
                continue;
            }

            while (packet_frames > 0 && !state->stop.load(std::memory_order_acquire)) {
                BYTE *data = nullptr;
                UINT32 num_frames = 0;
                DWORD flags = 0;
                hr = capture_client->GetBuffer(&data, &num_frames, &flags, nullptr, nullptr);
                if (hr == AUDCLNT_E_DEVICE_INVALIDATED || FAILED(hr)) {
                    packet_frames = 0;
                    break;
                }

                if (num_frames > 0) {
                    if ((flags & AUDCLNT_BUFFERFLAGS_SILENT) != 0 || !data) {
                        const size_t out_frames =
                            (fmt.sample_rate == kTargetSampleRate)
                                ? num_frames
                                : static_cast<size_t>(std::max<long long>(
                                      1,
                                      std::llround(
                                          static_cast<double>(num_frames) *
                                          static_cast<double>(kTargetSampleRate) /
                                          static_cast<double>(fmt.sample_rate))));
                        accum.insert(accum.end(), out_frames * kTargetChannels, 0);
                    } else {
                        convert_to_target_stereo_i16(data, num_frames, fmt, accum);
                    }
                }

                capture_client->ReleaseBuffer(num_frames);

                size_t offset = 0;
                while (accum.size() - offset >= kChunkSamples) {
                    const int16_t *chunk = accum.data() + offset;
                    if (is_all_zero(chunk, kChunkSamples)) {
                        if (silent_tail < kSilentTailChunks) {
                            ++silent_tail;
                            state->callback(state->ctx, chunk, kChunkSamples);
                        }
                    } else {
                        silent_tail = 0;
                        state->callback(state->ctx, chunk, kChunkSamples);
                    }
                    offset += kChunkSamples;
                }
                if (offset > 0) {
                    accum.erase(accum.begin(), accum.begin() + static_cast<ptrdiff_t>(offset));
                }

                hr = capture_client->GetNextPacketSize(&packet_frames);
                if (hr == AUDCLNT_E_DEVICE_INVALIDATED || FAILED(hr)) {
                    packet_frames = 0;
                    break;
                }
            }
            if (FAILED(hr)) {
                break;
            }
        }

        client->Stop();
        if (!device_switched && FAILED(hr) && !state->stop.load(std::memory_order_acquire)) {
            Sleep(100);
        }
    }
}

struct PlayerState {
    std::mutex lock;
    int16_t ring[kRingCapacity]{};
    size_t head = 0;
    size_t count = 0;
    std::atomic<bool> stop{false};
    std::thread worker;

    void push_samples(const int16_t *samples, size_t sample_count) {
        if (!samples || sample_count < 2) {
            return;
        }
        sample_count &= ~size_t(1);

        std::lock_guard<std::mutex> guard(lock);
        if (sample_count >= kMaxDriftSamples) {
            const size_t keep = kTargetDriftSamples & ~size_t(1);
            samples += (sample_count - keep);
            sample_count = keep;
            head = 0;
            count = 0;
        } else if (count + sample_count > kMaxDriftSamples) {
            size_t desired_existing = 0;
            if (sample_count < kTargetDriftSamples) {
                desired_existing = (kTargetDriftSamples - sample_count) & ~size_t(1);
            }
            if (count > desired_existing) {
                const size_t drop = (count - desired_existing) & ~size_t(1);
                head = (head + drop) % kRingCapacity;
                count -= drop;
            }
        }

        if (count + sample_count > kRingCapacity) {
            const size_t drop = (count + sample_count - kRingCapacity + 1) & ~size_t(1);
            const size_t actual_drop = std::min(drop, count);
            head = (head + actual_drop) % kRingCapacity;
            count -= actual_drop;
        }

        const size_t write_pos = (head + count) % kRingCapacity;
        const size_t first = kRingCapacity - write_pos;
        if (sample_count <= first) {
            std::memcpy(ring + write_pos, samples, sample_count * sizeof(int16_t));
        } else {
            std::memcpy(ring + write_pos, samples, first * sizeof(int16_t));
            std::memcpy(ring, samples + first, (sample_count - first) * sizeof(int16_t));
        }
        count += sample_count;
    }

    size_t pop_samples(int16_t *dst, size_t needed) {
        std::lock_guard<std::mutex> guard(lock);
        const size_t to_copy = std::min(count, needed);
        if (to_copy > 0) {
            const size_t first = kRingCapacity - head;
            if (to_copy <= first) {
                std::memcpy(dst, ring + head, to_copy * sizeof(int16_t));
            } else {
                std::memcpy(dst, ring + head, first * sizeof(int16_t));
                std::memcpy(dst + first, ring, (to_copy - first) * sizeof(int16_t));
            }
            head = (head + to_copy) % kRingCapacity;
            count -= to_copy;
        }
        if (to_copy < needed) {
            std::memset(dst + to_copy, 0, (needed - to_copy) * sizeof(int16_t));
        }
        return to_copy;
    }
};

void write_rendered_audio(
    BYTE *out,
    UINT32 out_frames,
    const FormatInfo &out_fmt,
    const int16_t *src_stereo,
    size_t src_frames
) {
    if (out_fmt.sample_rate == kTargetSampleRate && out_fmt.channels == kTargetChannels &&
        out_fmt.is_int16) {
        std::memcpy(out, src_stereo, static_cast<size_t>(out_frames) * 2 * sizeof(int16_t));
        return;
    }

    const double ratio =
        static_cast<double>(kTargetSampleRate) / static_cast<double>(out_fmt.sample_rate);
    for (UINT32 f = 0; f < out_frames; ++f) {
        float l = 0.0f;
        float r = 0.0f;
        if (src_frames > 0) {
            if (out_fmt.sample_rate == kTargetSampleRate) {
                const size_t idx = std::min<size_t>(f, src_frames - 1);
                l = static_cast<float>(src_stereo[idx * 2]) / 32768.0f;
                r = static_cast<float>(src_stereo[idx * 2 + 1]) / 32768.0f;
            } else {
                const double pos = static_cast<double>(f) * ratio;
                size_t idx0 = static_cast<size_t>(pos);
                if (idx0 >= src_frames) {
                    idx0 = src_frames - 1;
                }
                const size_t idx1 = (idx0 + 1 < src_frames) ? (idx0 + 1) : idx0;
                const float frac = static_cast<float>(pos - static_cast<double>(idx0));
                const float l0 = static_cast<float>(src_stereo[idx0 * 2]) / 32768.0f;
                const float l1 = static_cast<float>(src_stereo[idx1 * 2]) / 32768.0f;
                const float r0 = static_cast<float>(src_stereo[idx0 * 2 + 1]) / 32768.0f;
                const float r1 = static_cast<float>(src_stereo[idx1 * 2 + 1]) / 32768.0f;
                l = l0 + (l1 - l0) * frac;
                r = r0 + (r1 - r0) * frac;
            }
        }

        const float mono = 0.5f * (l + r);
        for (uint16_t ch = 0; ch < out_fmt.channels; ++ch) {
            float sample = 0.0f;
            if (out_fmt.channels == 1) {
                sample = mono;
            } else if (ch == 0) {
                sample = l;
            } else if (ch == 1) {
                sample = r;
            }
            const size_t idx = static_cast<size_t>(f) * out_fmt.channels + ch;
            if (out_fmt.is_float) {
                reinterpret_cast<float *>(out)[idx] = sample;
            } else {
                reinterpret_cast<int16_t *>(out)[idx] = float_to_i16(sample);
            }
        }
    }
}

void player_thread_main(PlayerState *state) {
    CoInitGuard co_init;
    if (!co_init.initialized) {
        return;
    }

    std::vector<int16_t> scratch;
    while (!state->stop.load(std::memory_order_acquire)) {
        ComPtr<IMMDeviceEnumerator> enumerator;
        HRESULT hr = CoCreateInstance(
            __uuidof(MMDeviceEnumerator),
            nullptr,
            CLSCTX_ALL,
            __uuidof(IMMDeviceEnumerator),
            reinterpret_cast<void **>(enumerator.put()));
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        ComPtr<IMMDevice> device;
        hr = enumerator->GetDefaultAudioEndpoint(eRender, eConsole, device.put());
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }
        std::wstring active_device_id;
        get_device_id(device.get(), active_device_id);

        ComPtr<IAudioClient> client;
        hr = device->Activate(
            __uuidof(IAudioClient),
            CLSCTX_ALL,
            nullptr,
            reinterpret_cast<void **>(client.put()));
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        FormatInfo fmt{};
        WAVEFORMATEX target = make_target_format();
        hr = client->Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            400000, // 40 ms
            0,
            &target,
            nullptr);
        if (SUCCEEDED(hr)) {
            fmt.sample_rate = kTargetSampleRate;
            fmt.channels = kTargetChannels;
            fmt.is_int16 = true;
            fmt.is_float = false;
        } else {
            // Re-activate a fresh IAudioClient because a failed Initialize can invalidate it.
            hr = device->Activate(
                __uuidof(IAudioClient),
                CLSCTX_ALL,
                nullptr,
                reinterpret_cast<void **>(client.put()));
            if (FAILED(hr)) {
                sleep_interruptible(state->stop, 1000);
                continue;
            }
            CoTaskMemGuard mix_guard;
            hr = client->GetMixFormat(&mix_guard.ptr);
            if (FAILED(hr) || !parse_wave_format(mix_guard.ptr, fmt)) {
                sleep_interruptible(state->stop, 1000);
                continue;
            }
            hr = client->Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                0,
                400000,
                0,
                mix_guard.ptr,
                nullptr);
            if (FAILED(hr)) {
                sleep_interruptible(state->stop, 1000);
                continue;
            }
        }

        UINT32 buffer_frames = 0;
        hr = client->GetBufferSize(&buffer_frames);
        if (FAILED(hr) || buffer_frames == 0) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        ComPtr<IAudioRenderClient> render_client;
        hr = client->GetService(
            __uuidof(IAudioRenderClient),
            reinterpret_cast<void **>(render_client.put()));
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        hr = client->Start();
        if (FAILED(hr)) {
            sleep_interruptible(state->stop, 1000);
            continue;
        }

        bool device_switched = false;
        ULONGLONG last_default_check = GetTickCount64();
        while (!state->stop.load(std::memory_order_acquire)) {
            const ULONGLONG now = GetTickCount64();
            if (now - last_default_check >= 500) {
                last_default_check = now;
                if (default_render_device_changed(enumerator.get(), active_device_id)) {
                    device_switched = true;
                    break;
                }
            }
            UINT32 padding = 0;
            hr = client->GetCurrentPadding(&padding);
            if (hr == AUDCLNT_E_DEVICE_INVALIDATED || hr == AUDCLNT_E_SERVICE_NOT_RUNNING ||
                FAILED(hr)) {
                break;
            }
            if (padding >= buffer_frames) {
                Sleep(5);
                continue;
            }
            const UINT32 available = buffer_frames - padding;
            const size_t src_frames =
                (fmt.sample_rate == kTargetSampleRate)
                    ? available
                    : static_cast<size_t>(std::max<long long>(
                          1,
                          std::llround(
                              static_cast<double>(available) *
                              static_cast<double>(kTargetSampleRate) /
                              static_cast<double>(fmt.sample_rate))));
            const size_t needed_samples = src_frames * kTargetChannels;
            scratch.resize(needed_samples);
            const size_t copied = state->pop_samples(scratch.data(), needed_samples);

            BYTE *out = nullptr;
            hr = render_client->GetBuffer(available, &out);
            if (hr == AUDCLNT_E_DEVICE_INVALIDATED || FAILED(hr)) {
                break;
            }
            if (copied == 0) {
                render_client->ReleaseBuffer(available, AUDCLNT_BUFFERFLAGS_SILENT);
            } else {
                write_rendered_audio(out, available, fmt, scratch.data(), src_frames);
                render_client->ReleaseBuffer(available, 0);
            }
            Sleep(5);
        }

        client->Stop();
        if (!device_switched && FAILED(hr) && !state->stop.load(std::memory_order_acquire)) {
            Sleep(100);
        }
    }
}

} // namespace

extern "C" {

void *remoters_win_audio_capture_start(AudioCaptureCallback callback, void *ctx) {
    if (!callback) {
        return nullptr;
    }
    auto *state = new (std::nothrow) CaptureState();
    if (!state) {
        return nullptr;
    }
    state->callback = callback;
    state->ctx = ctx;
    state->worker = std::thread(capture_thread_main, state);
    return state;
}

void remoters_win_audio_capture_stop(void *ptr) {
    if (!ptr) {
        return;
    }
    auto *state = static_cast<CaptureState *>(ptr);
    state->stop.store(true, std::memory_order_release);
    if (state->worker.joinable()) {
        state->worker.join();
    }
    delete state;
}

void *remoters_win_audio_player_create(void) {
    auto *state = new (std::nothrow) PlayerState();
    if (!state) {
        return nullptr;
    }
    state->worker = std::thread(player_thread_main, state);
    return state;
}

void remoters_win_audio_player_push(void *ptr, const int16_t *samples, size_t sample_count) {
    if (!ptr) {
        return;
    }
    static_cast<PlayerState *>(ptr)->push_samples(samples, sample_count);
}

void remoters_win_audio_player_destroy(void *ptr) {
    if (!ptr) {
        return;
    }
    auto *state = static_cast<PlayerState *>(ptr);
    state->stop.store(true, std::memory_order_release);
    if (state->worker.joinable()) {
        state->worker.join();
    }
    delete state;
}

} // extern "C"
