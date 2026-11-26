#define _GNU_SOURCE
#include <errno.h>
#include <pipewire/pipewire.h>
#include <pthread.h>
#include <spa/param/video/format-utils.h>
#include <spa/utils/result.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

enum RemotersPwFourcc {
    REMOTERS_PW_BGR0 = 1,
    REMOTERS_PW_BGRA = 2,
    REMOTERS_PW_RGB0 = 3,
    REMOTERS_PW_RGBA = 4,
};

#define REMOTERS_PW_SLOT_COUNT 3

typedef struct RemotersPwSlot {
    uint8_t *data;
    size_t cap;
    size_t len;
    uint32_t width;
    uint32_t height;
    uint32_t fourcc;
    uint64_t seq;
    uint64_t capture_ns;
} RemotersPwSlot;

typedef struct RemotersPwStream {
    struct pw_thread_loop *loop;
    struct pw_context *context;
    struct pw_core *core;
    struct pw_stream *stream;
    struct spa_hook stream_listener;
    struct spa_hook core_listener;

    pthread_mutex_t mu;
    pthread_cond_t cv;
    clockid_t cv_clock;

    uint32_t width;
    uint32_t height;
    uint32_t fourcc;
    bool has_format;
    bool started;
    bool closed;

    /* Triple-buffered latest-frame mailbox:
     * - latest_idx: most recently completed frame (-1 before first frame)
     * - read_idx: slot currently borrowed by the encoder (-1 when idle)
     * - PipeWire writes into whichever slot is neither latest_idx nor read_idx,
     *   performing all memcpy/realloc work outside mu so callbacks never stall. */
    RemotersPwSlot slots[REMOTERS_PW_SLOT_COUNT];
    int latest_idx;
    int read_idx;
    uint64_t seq;
} RemotersPwStream;

static uint64_t now_monotonic_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

static uint32_t map_spa_format(uint32_t fmt) {
    switch (fmt) {
        case SPA_VIDEO_FORMAT_BGRx:
            return REMOTERS_PW_BGR0;
        case SPA_VIDEO_FORMAT_BGRA:
            return REMOTERS_PW_BGRA;
        case SPA_VIDEO_FORMAT_RGBx:
            return REMOTERS_PW_RGB0;
        case SPA_VIDEO_FORMAT_RGBA:
            return REMOTERS_PW_RGBA;
        default:
            return 0;
    }
}

static void on_core_error(void *data, uint32_t id, int seq, int res, const char *message) {
    fprintf(
        stderr,
        "remoters-pw: core error id=%u seq=%d res=%d (%s): %s\n",
        id,
        seq,
        res,
        spa_strerror(res),
        message ? message : ""
    );
    RemotersPwStream *s = (RemotersPwStream *)data;
    pthread_mutex_lock(&s->mu);
    s->closed = true;
    pthread_cond_broadcast(&s->cv);
    pthread_mutex_unlock(&s->mu);
}

static const struct pw_core_events core_events = {
    PW_VERSION_CORE_EVENTS,
    .error = on_core_error,
};

static void on_stream_state_changed(
    void *data,
    enum pw_stream_state old_state,
    enum pw_stream_state state,
    const char *error
) {
    (void)old_state;
    if (state == PW_STREAM_STATE_ERROR) {
        fprintf(
            stderr,
            "remoters-pw: stream error: %s\n",
            error ? error : "unknown"
        );
    }
    RemotersPwStream *s = (RemotersPwStream *)data;
    pthread_mutex_lock(&s->mu);
    if (state == PW_STREAM_STATE_STREAMING) {
        s->started = true;
    } else if (state == PW_STREAM_STATE_ERROR ||
               (state == PW_STREAM_STATE_UNCONNECTED && s->started)) {
        s->closed = true;
        pthread_cond_broadcast(&s->cv);
    }
    pthread_mutex_unlock(&s->mu);
}

static void on_stream_param_changed(void *data, uint32_t id, const struct spa_pod *param) {
    if (!param || id != SPA_PARAM_Format) {
        return;
    }
    RemotersPwStream *s = (RemotersPwStream *)data;
    uint32_t media_type = 0;
    uint32_t media_subtype = 0;
    if (spa_format_parse(param, &media_type, &media_subtype) < 0 ||
        media_type != SPA_MEDIA_TYPE_video ||
        media_subtype != SPA_MEDIA_SUBTYPE_raw) {
        return;
    }
    struct spa_video_info_raw info;
    memset(&info, 0, sizeof(info));
    if (spa_format_video_raw_parse(param, &info) < 0) {
        return;
    }
    uint32_t fourcc = map_spa_format(info.format);
    if (fourcc == 0 || info.size.width < 2 || info.size.height < 2) {
        return;
    }

    pthread_mutex_lock(&s->mu);
    s->width = info.size.width;
    s->height = info.size.height;
    s->fourcc = fourcc;
    s->has_format = true;
    pthread_mutex_unlock(&s->mu);

    uint8_t buf[512];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
    const struct spa_pod *params[1];
    /* Do not pin exact size/stride on the consumer side so compositors
     * (e.g. GNOME/Mutter, KWin, wlroots) can allocate aligned SHM/MemFd
     * buffers; copy_pw_buffer handles arbitrary d->chunk->stride values. */
    params[0] = spa_pod_builder_add_object(
        &b,
        SPA_TYPE_OBJECT_ParamBuffers,
        SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_buffers,
        SPA_POD_CHOICE_RANGE_Int(4, 1, 32),
        SPA_PARAM_BUFFERS_blocks,
        SPA_POD_Int(1),
        SPA_PARAM_BUFFERS_dataType,
        SPA_POD_CHOICE_FLAGS_Int((1 << SPA_DATA_MemFd) | (1 << SPA_DATA_MemPtr))
    );
    int res = pw_stream_update_params(s->stream, params, 1);
    if (res < 0) {
        fprintf(
            stderr,
            "remoters-pw: pw_stream_update_params(Buffers) failed: %d (%s)\n",
            res,
            spa_strerror(res)
        );
    }
}

static int pick_write_slot_locked(const RemotersPwStream *s) {
    for (int i = 0; i < REMOTERS_PW_SLOT_COUNT; i++) {
        if (i != s->latest_idx && i != s->read_idx) {
            return i;
        }
    }
    return 0;
}

static void copy_pw_buffer(RemotersPwStream *s, struct pw_buffer *pw_buf) {
    uint64_t capture_ns = now_monotonic_ns();
    struct spa_buffer *buf = pw_buf->buffer;
    if (!buf || buf->n_datas < 1) {
        return;
    }
    struct spa_data *d = &buf->datas[0];
    if (!d->chunk || d->chunk->size == 0 || (d->chunk->flags & SPA_CHUNK_FLAG_CORRUPTED)) {
        return;
    }
    uint8_t *mapped = NULL;
    const uint8_t *src = (const uint8_t *)d->data;
    size_t map_size = 0;
    if (!src && d->type == SPA_DATA_MemFd && d->fd >= 0 && d->maxsize > 0) {
        map_size = (size_t)d->mapoffset + (size_t)d->maxsize;
        void *p = mmap(NULL, map_size, PROT_READ, MAP_SHARED, (int)d->fd, 0);
        if (p != MAP_FAILED) {
            mapped = (uint8_t *)p;
            src = mapped + d->mapoffset;
        }
    }
    if (!src) {
        return;
    }

    pthread_mutex_lock(&s->mu);
    if (!s->has_format || s->width < 2 || s->height < 2) {
        pthread_mutex_unlock(&s->mu);
        if (mapped) {
            munmap(mapped, map_size);
        }
        return;
    }
    uint32_t w = s->width;
    uint32_t h = s->height;
    uint32_t fourcc = s->fourcc;
    int w_idx = pick_write_slot_locked(s);
    pthread_mutex_unlock(&s->mu);

    size_t row_bytes = (size_t)w * 4;
    size_t needed = row_bytes * (size_t)h;
    size_t src_stride = d->chunk->stride > 0 ? (size_t)d->chunk->stride : row_bytes;
    size_t offset = (size_t)d->chunk->offset;
    size_t available = d->maxsize > offset ? (size_t)d->maxsize - offset : 0;

    bool copied = false;
    if (src_stride >= row_bytes && available >= src_stride * (size_t)(h - 1) + row_bytes) {
        RemotersPwSlot *slot = &s->slots[w_idx];
        if (slot->cap < needed) {
            uint8_t *grown = (uint8_t *)realloc(slot->data, needed);
            if (grown) {
                slot->data = grown;
                slot->cap = needed;
            }
        }
        if (slot->data && slot->cap >= needed) {
            const uint8_t *row_ptr = src + offset;
            if (src_stride == row_bytes) {
                memcpy(slot->data, row_ptr, needed);
            } else {
                for (uint32_t y = 0; y < h; y++) {
                    memcpy(
                        slot->data + (size_t)y * row_bytes,
                        row_ptr + (size_t)y * src_stride,
                        row_bytes
                    );
                }
            }
            slot->len = needed;
            slot->width = w;
            slot->height = h;
            slot->fourcc = fourcc;
            slot->capture_ns = capture_ns;
            copied = true;
        }
    }

    if (mapped) {
        munmap(mapped, map_size);
    }

    if (copied) {
        pthread_mutex_lock(&s->mu);
        s->seq += 1;
        s->slots[w_idx].seq = s->seq;
        s->latest_idx = w_idx;
        pthread_cond_broadcast(&s->cv);
        pthread_mutex_unlock(&s->mu);
    }
}

static void on_stream_process(void *data) {
    RemotersPwStream *s = (RemotersPwStream *)data;
    struct pw_buffer *latest = NULL;
    struct pw_buffer *next = NULL;
    while ((next = pw_stream_dequeue_buffer(s->stream)) != NULL) {
        if (latest) {
            pw_stream_queue_buffer(s->stream, latest);
        }
        latest = next;
    }
    if (latest) {
        copy_pw_buffer(s, latest);
        pw_stream_queue_buffer(s->stream, latest);
    }
}

static const struct pw_stream_events stream_events = {
    PW_VERSION_STREAM_EVENTS,
    .state_changed = on_stream_state_changed,
    .param_changed = on_stream_param_changed,
    .process = on_stream_process,
};

static void fill_deadline(const RemotersPwStream *s, struct timespec *ts, uint32_t timeout_ms) {
    clock_gettime(s->cv_clock, ts);
    ts->tv_sec += timeout_ms / 1000;
    ts->tv_nsec += (long)(timeout_ms % 1000) * 1000000L;
    if (ts->tv_nsec >= 1000000000L) {
        ts->tv_sec += 1;
        ts->tv_nsec -= 1000000000L;
    }
}

void remoters_pw_stop(RemotersPwStream *s) {
    if (!s) {
        return;
    }
    if (s->loop) {
        pw_thread_loop_lock(s->loop);
        if (s->stream) {
            pw_stream_disconnect(s->stream);
            pw_stream_destroy(s->stream);
            s->stream = NULL;
        }
        if (s->core) {
            pw_core_disconnect(s->core);
            s->core = NULL;
        }
        if (s->context) {
            pw_context_destroy(s->context);
            s->context = NULL;
        }
        pw_thread_loop_unlock(s->loop);
        pw_thread_loop_stop(s->loop);
        pw_thread_loop_destroy(s->loop);
        s->loop = NULL;
    }
    pthread_mutex_destroy(&s->mu);
    pthread_cond_destroy(&s->cv);
    for (int i = 0; i < REMOTERS_PW_SLOT_COUNT; i++) {
        free(s->slots[i].data);
    }
    free(s);
}

RemotersPwStream *remoters_pw_start(
    int pw_fd,
    uint32_t node_id,
    uint32_t *out_width,
    uint32_t *out_height,
    uint32_t *out_fourcc
) {
    if (pw_fd < 0 || !out_width || !out_height || !out_fourcc) {
        return NULL;
    }
    int dup_fd = dup(pw_fd);
    if (dup_fd < 0) {
        return NULL;
    }

    pw_init(NULL, NULL);

    RemotersPwStream *s = (RemotersPwStream *)calloc(1, sizeof(RemotersPwStream));
    if (!s) {
        close(dup_fd);
        return NULL;
    }
    s->latest_idx = -1;
    s->read_idx = -1;
    pthread_mutex_init(&s->mu, NULL);

    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    if (pthread_condattr_setclock(&attr, CLOCK_MONOTONIC) == 0) {
        s->cv_clock = CLOCK_MONOTONIC;
    } else {
        s->cv_clock = CLOCK_REALTIME;
    }
    pthread_cond_init(&s->cv, &attr);
    pthread_condattr_destroy(&attr);

    s->loop = pw_thread_loop_new("remoters-pw", NULL);
    if (!s->loop) {
        close(dup_fd);
        remoters_pw_stop(s);
        return NULL;
    }
    s->context = pw_context_new(pw_thread_loop_get_loop(s->loop), NULL, 0);
    if (!s->context) {
        close(dup_fd);
        remoters_pw_stop(s);
        return NULL;
    }

    pw_thread_loop_lock(s->loop);
    s->core = pw_context_connect_fd(s->context, dup_fd, NULL, 0);
    if (!s->core) {
        pw_thread_loop_unlock(s->loop);
        remoters_pw_stop(s);
        return NULL;
    }
    pw_core_add_listener(s->core, &s->core_listener, &core_events, s);

    struct pw_properties *props = pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Video",
        PW_KEY_MEDIA_CATEGORY, "Capture",
        PW_KEY_MEDIA_ROLE, "Screen",
        NULL
    );
    s->stream = pw_stream_new(s->core, "remoters-capture", props);
    if (!s->stream) {
        pw_thread_loop_unlock(s->loop);
        remoters_pw_stop(s);
        return NULL;
    }
    pw_stream_add_listener(s->stream, &s->stream_listener, &stream_events, s);

    uint8_t buf[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buf, sizeof(buf));
    const struct spa_pod *params[1];
    /* Allow variable framerate (0/1, used by GNOME/Mutter, KWin, and wlroots
     * ScreenCast producers alongside maxFramerate) as well as fixed rates.
     * Omit SPA_FORMAT_VIDEO_modifier so PipeWire selects unmodified SHM/MemFd
     * formats rather than GPU-tiled DMABuf modifiers. */
    params[0] = spa_pod_builder_add_object(
        &b,
        SPA_TYPE_OBJECT_Format,
        SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType,
        SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype,
        SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format,
        SPA_POD_CHOICE_ENUM_Id(
            5,
            SPA_VIDEO_FORMAT_BGRx,
            SPA_VIDEO_FORMAT_BGRx,
            SPA_VIDEO_FORMAT_BGRA,
            SPA_VIDEO_FORMAT_RGBx,
            SPA_VIDEO_FORMAT_RGBA
        ),
        SPA_FORMAT_VIDEO_size,
        SPA_POD_CHOICE_RANGE_Rectangle(
            &SPA_RECTANGLE(1920, 1080),
            &SPA_RECTANGLE(2, 2),
            &SPA_RECTANGLE(16384, 16384)
        ),
        SPA_FORMAT_VIDEO_framerate,
        SPA_POD_CHOICE_RANGE_Fraction(
            &SPA_FRACTION(0, 1),
            &SPA_FRACTION(0, 1),
            &SPA_FRACTION(1000, 1)
        )
    );

    enum pw_stream_flags flags =
        (enum pw_stream_flags)(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS);
    int res = pw_stream_connect(s->stream, PW_DIRECTION_INPUT, node_id, flags, params, 1);
    pw_thread_loop_unlock(s->loop);
    if (res < 0 || pw_thread_loop_start(s->loop) < 0) {
        remoters_pw_stop(s);
        return NULL;
    }

    struct timespec deadline;
    fill_deadline(s, &deadline, 5000);
    pthread_mutex_lock(&s->mu);
    while (s->seq == 0 && !s->closed) {
        if (pthread_cond_timedwait(&s->cv, &s->mu, &deadline) == ETIMEDOUT) {
            break;
        }
    }
    bool ready = s->seq > 0 && s->latest_idx >= 0 && s->has_format && !s->closed;
    if (ready) {
        const RemotersPwSlot *slot = &s->slots[s->latest_idx];
        *out_width = slot->width;
        *out_height = slot->height;
        *out_fourcc = slot->fourcc;
    }
    pthread_mutex_unlock(&s->mu);

    if (!ready) {
        remoters_pw_stop(s);
        return NULL;
    }
    return s;
}

/* Borrows the latest captured frame slot directly without copying.
 * Returns:
 *   1  -> new frame acquired (caller must call remoters_pw_release_frame)
 *   2  -> repeat/keepalive of latest frame acquired when allow_repeat != 0
 *         (caller must call remoters_pw_release_frame)
 *   0  -> timed out with no new frame
 *  -1  -> stream closed or invalid arguments */
int remoters_pw_acquire_frame(
    RemotersPwStream *s,
    uint64_t *last_seq,
    const uint8_t **out_ptr,
    size_t *out_len,
    uint32_t *out_w,
    uint32_t *out_h,
    uint32_t *out_fourcc,
    uint64_t *out_age_us,
    uint32_t timeout_ms,
    int allow_repeat
) {
    if (!s || !last_seq || !out_ptr || !out_len || !out_w || !out_h || !out_fourcc) {
        return -1;
    }
    struct timespec deadline;
    fill_deadline(s, &deadline, timeout_ms);

    pthread_mutex_lock(&s->mu);
    s->read_idx = -1;
    while (s->seq <= *last_seq && !s->closed) {
        if (timeout_ms == 0) {
            break;
        }
        if (pthread_cond_timedwait(&s->cv, &s->mu, &deadline) == ETIMEDOUT) {
            break;
        }
    }
    if (s->closed && s->seq <= *last_seq) {
        pthread_mutex_unlock(&s->mu);
        return -1;
    }
    bool is_new = s->seq > *last_seq;
    if (!is_new && (!allow_repeat || s->latest_idx < 0)) {
        pthread_mutex_unlock(&s->mu);
        return 0;
    }
    int idx = s->latest_idx;
    if (idx < 0 || !s->slots[idx].data || s->slots[idx].len == 0) {
        pthread_mutex_unlock(&s->mu);
        return 0;
    }
    s->read_idx = idx;
    const RemotersPwSlot *slot = &s->slots[idx];
    *out_ptr = slot->data;
    *out_len = slot->len;
    *out_w = slot->width;
    *out_h = slot->height;
    *out_fourcc = slot->fourcc;
    if (is_new) {
        *last_seq = slot->seq;
    }
    if (out_age_us) {
        if (is_new) {
            uint64_t now_ns = now_monotonic_ns();
            *out_age_us = now_ns > slot->capture_ns ? (now_ns - slot->capture_ns) / 1000ULL : 0ULL;
        } else {
            *out_age_us = 0ULL;
        }
    }
    pthread_mutex_unlock(&s->mu);
    return is_new ? 1 : 2;
}

void remoters_pw_release_frame(RemotersPwStream *s) {
    if (!s) {
        return;
    }
    pthread_mutex_lock(&s->mu);
    s->read_idx = -1;
    pthread_mutex_unlock(&s->mu);
}

int remoters_pw_read_frame(
    RemotersPwStream *s,
    uint64_t *last_seq,
    uint8_t *dst,
    size_t dst_cap,
    uint32_t *out_w,
    uint32_t *out_h,
    uint32_t *out_fourcc,
    uint32_t timeout_ms
) {
    if (!dst) {
        return -1;
    }
    const uint8_t *src = NULL;
    size_t src_len = 0;
    uint64_t saved_seq = last_seq ? *last_seq : 0;
    int status = remoters_pw_acquire_frame(
        s,
        last_seq,
        &src,
        &src_len,
        out_w,
        out_h,
        out_fourcc,
        NULL,
        timeout_ms,
        0
    );
    if (status != 1) {
        return status;
    }
    if (src_len > dst_cap) {
        *last_seq = saved_seq;
        remoters_pw_release_frame(s);
        return -2;
    }
    memcpy(dst, src, src_len);
    remoters_pw_release_frame(s);
    return 1;
}
