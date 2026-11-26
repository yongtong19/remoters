#import <AudioToolbox/AudioToolbox.h>
#import <CoreMedia/CoreMedia.h>
#import <Foundation/Foundation.h>
#include <math.h>
#include <pthread.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

enum {
    REMOTERS_AUDIO_SAMPLE_RATE = 48000,
    REMOTERS_AUDIO_CHANNELS = 2,
    REMOTERS_AUDIO_BUFFER_FRAMES = 960,
    REMOTERS_AUDIO_BUFFER_SAMPLES = REMOTERS_AUDIO_BUFFER_FRAMES * REMOTERS_AUDIO_CHANNELS,
    REMOTERS_AUDIO_NUM_BUFFERS = 5,
    REMOTERS_AUDIO_RING_CAPACITY = 24000, // 250 ms at 48 kHz stereo
    REMOTERS_AUDIO_TARGET_SAMPLES = 3840, // 40 ms at 48 kHz stereo
    REMOTERS_AUDIO_MAX_SAMPLES = 9600,    // 100 ms max drift at 48 kHz stereo
};

typedef struct {
    AudioQueueRef queue;
    AudioQueueBufferRef buffers[REMOTERS_AUDIO_NUM_BUFFERS];
    pthread_mutex_t lock;
    bool closing;
    int16_t ring[REMOTERS_AUDIO_RING_CAPACITY];
    size_t head;
    size_t count;
} RemotersMacosAudioPlayer;

typedef void (*RemotersMacosAudioCallback)(void *ctx, const int16_t *samples, size_t sample_count);

static void player_output_callback(
    void *user_data,
    AudioQueueRef queue,
    AudioQueueBufferRef buffer
) {
    RemotersMacosAudioPlayer *player = (RemotersMacosAudioPlayer *)user_data;
    if (!player || !buffer) {
        return;
    }

    const size_t capacity_samples = buffer->mAudioDataBytesCapacity / sizeof(int16_t);
    int16_t *dst = (int16_t *)buffer->mAudioData;

    pthread_mutex_lock(&player->lock);
    if (player->closing) {
        pthread_mutex_unlock(&player->lock);
        return;
    }

    const size_t to_copy = player->count < capacity_samples ? player->count : capacity_samples;
    if (to_copy > 0) {
        const size_t first = REMOTERS_AUDIO_RING_CAPACITY - player->head;
        if (to_copy <= first) {
            memcpy(dst, player->ring + player->head, to_copy * sizeof(int16_t));
        } else {
            memcpy(dst, player->ring + player->head, first * sizeof(int16_t));
            memcpy(dst + first, player->ring, (to_copy - first) * sizeof(int16_t));
        }
        player->head = (player->head + to_copy) % REMOTERS_AUDIO_RING_CAPACITY;
        player->count -= to_copy;
    }
    pthread_mutex_unlock(&player->lock);

    if (to_copy < capacity_samples) {
        memset(dst + to_copy, 0, (capacity_samples - to_copy) * sizeof(int16_t));
    }
    buffer->mAudioDataByteSize = (UInt32)(capacity_samples * sizeof(int16_t));
    AudioQueueEnqueueBuffer(queue, buffer, 0, NULL);
}

void *remoters_macos_audio_player_create(void) {
    RemotersMacosAudioPlayer *player =
        (RemotersMacosAudioPlayer *)calloc(1, sizeof(RemotersMacosAudioPlayer));
    if (!player) {
        return NULL;
    }
    if (pthread_mutex_init(&player->lock, NULL) != 0) {
        free(player);
        return NULL;
    }

    AudioStreamBasicDescription fmt;
    memset(&fmt, 0, sizeof(fmt));
    fmt.mSampleRate = (Float64)REMOTERS_AUDIO_SAMPLE_RATE;
    fmt.mFormatID = kAudioFormatLinearPCM;
    fmt.mFormatFlags = kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked;
    fmt.mBitsPerChannel = 16;
    fmt.mChannelsPerFrame = REMOTERS_AUDIO_CHANNELS;
    fmt.mBytesPerFrame = sizeof(int16_t) * REMOTERS_AUDIO_CHANNELS;
    fmt.mFramesPerPacket = 1;
    fmt.mBytesPerPacket = fmt.mBytesPerFrame;

    OSStatus status = AudioQueueNewOutput(
        &fmt,
        player_output_callback,
        player,
        NULL,
        NULL,
        0,
        &player->queue
    );
    if (status != noErr || !player->queue) {
        pthread_mutex_destroy(&player->lock);
        free(player);
        return NULL;
    }

    const UInt32 buffer_bytes = REMOTERS_AUDIO_BUFFER_SAMPLES * (UInt32)sizeof(int16_t);
    for (int i = 0; i < REMOTERS_AUDIO_NUM_BUFFERS; ++i) {
        status = AudioQueueAllocateBuffer(player->queue, buffer_bytes, &player->buffers[i]);
        if (status != noErr || !player->buffers[i]) {
            AudioQueueDispose(player->queue, true);
            pthread_mutex_destroy(&player->lock);
            free(player);
            return NULL;
        }
        memset(player->buffers[i]->mAudioData, 0, buffer_bytes);
        player->buffers[i]->mAudioDataByteSize = buffer_bytes;
        status = AudioQueueEnqueueBuffer(player->queue, player->buffers[i], 0, NULL);
        if (status != noErr) {
            AudioQueueDispose(player->queue, true);
            pthread_mutex_destroy(&player->lock);
            free(player);
            return NULL;
        }
    }

    status = AudioQueueStart(player->queue, NULL);
    if (status != noErr) {
        AudioQueueDispose(player->queue, true);
        pthread_mutex_destroy(&player->lock);
        free(player);
        return NULL;
    }

    return player;
}

void remoters_macos_audio_player_push(void *ptr, const int16_t *samples, size_t sample_count) {
    if (!ptr || !samples || sample_count < 2) {
        return;
    }
    // Keep stereo frame alignment.
    sample_count &= ~(size_t)1;

    RemotersMacosAudioPlayer *player = (RemotersMacosAudioPlayer *)ptr;
    pthread_mutex_lock(&player->lock);
    if (player->closing) {
        pthread_mutex_unlock(&player->lock);
        return;
    }

    if (sample_count >= REMOTERS_AUDIO_MAX_SAMPLES) {
        const size_t keep = REMOTERS_AUDIO_TARGET_SAMPLES & ~(size_t)1;
        samples += (sample_count - keep);
        sample_count = keep;
        player->head = 0;
        player->count = 0;
    } else if (player->count + sample_count > REMOTERS_AUDIO_MAX_SAMPLES) {
        size_t desired_existing = 0;
        if (sample_count < REMOTERS_AUDIO_TARGET_SAMPLES) {
            desired_existing = (REMOTERS_AUDIO_TARGET_SAMPLES - sample_count) & ~(size_t)1;
        }
        if (player->count > desired_existing) {
            const size_t drop = (player->count - desired_existing) & ~(size_t)1;
            player->head = (player->head + drop) % REMOTERS_AUDIO_RING_CAPACITY;
            player->count -= drop;
        }
    }

    if (player->count + sample_count > REMOTERS_AUDIO_RING_CAPACITY) {
        const size_t drop =
            (player->count + sample_count - REMOTERS_AUDIO_RING_CAPACITY + 1) & ~(size_t)1;
        const size_t actual_drop = drop < player->count ? drop : player->count;
        player->head = (player->head + actual_drop) % REMOTERS_AUDIO_RING_CAPACITY;
        player->count -= actual_drop;
    }

    const size_t write_pos = (player->head + player->count) % REMOTERS_AUDIO_RING_CAPACITY;
    const size_t first = REMOTERS_AUDIO_RING_CAPACITY - write_pos;
    if (sample_count <= first) {
        memcpy(player->ring + write_pos, samples, sample_count * sizeof(int16_t));
    } else {
        memcpy(player->ring + write_pos, samples, first * sizeof(int16_t));
        memcpy(player->ring, samples + first, (sample_count - first) * sizeof(int16_t));
    }
    player->count += sample_count;
    pthread_mutex_unlock(&player->lock);
}

void remoters_macos_audio_player_destroy(void *ptr) {
    if (!ptr) {
        return;
    }
    RemotersMacosAudioPlayer *player = (RemotersMacosAudioPlayer *)ptr;
    pthread_mutex_lock(&player->lock);
    player->closing = true;
    pthread_mutex_unlock(&player->lock);

    if (player->queue) {
        AudioQueueStop(player->queue, true);
        AudioQueueDispose(player->queue, true);
        player->queue = NULL;
    }
    pthread_mutex_destroy(&player->lock);
    free(player);
}

static inline int16_t float_to_i16(float value) {
    if (!(value == value)) {
        return 0;
    }
    if (value >= 1.0f) {
        return 32767;
    }
    if (value <= -1.0f) {
        return -32768;
    }
    const long rounded = lrintf(value * 32767.0f);
    if (rounded > 32767) {
        return 32767;
    }
    if (rounded < -32768) {
        return -32768;
    }
    return (int16_t)rounded;
}

static inline float read_normalized_sample(
    const AudioBufferList *abl,
    bool is_float,
    bool is_planar,
    uint32_t channels,
    size_t frame,
    uint32_t channel
) {
    const uint32_t ch = channel < channels ? channel : 0;
    if (is_planar) {
        if (ch >= abl->mNumberBuffers || !abl->mBuffers[ch].mData) {
            return 0.0f;
        }
        if (is_float) {
            const float *samples = (const float *)abl->mBuffers[ch].mData;
            return samples[frame];
        } else {
            const int16_t *samples = (const int16_t *)abl->mBuffers[ch].mData;
            return (float)samples[frame] / 32768.0f;
        }
    } else {
        if (abl->mNumberBuffers == 0 || !abl->mBuffers[0].mData) {
            return 0.0f;
        }
        const size_t idx = frame * (size_t)channels + (size_t)ch;
        if (is_float) {
            const float *samples = (const float *)abl->mBuffers[0].mData;
            return samples[idx];
        } else {
            const int16_t *samples = (const int16_t *)abl->mBuffers[0].mData;
            return (float)samples[idx] / 32768.0f;
        }
    }
}

bool remoters_macos_extract_audio(
    CMSampleBufferRef sample_buffer,
    RemotersMacosAudioCallback callback,
    void *ctx
) {
    if (!sample_buffer || !callback) {
        return false;
    }
    if (!CMSampleBufferIsValid(sample_buffer) || !CMSampleBufferDataIsReady(sample_buffer)) {
        return false;
    }

    CMFormatDescriptionRef fmt_desc = CMSampleBufferGetFormatDescription(sample_buffer);
    if (!fmt_desc) {
        return false;
    }
    const AudioStreamBasicDescription *asbd =
        CMAudioFormatDescriptionGetStreamBasicDescription(fmt_desc);
    if (!asbd || asbd->mFormatID != kAudioFormatLinearPCM || asbd->mChannelsPerFrame == 0 ||
        asbd->mSampleRate <= 0.0) {
        return false;
    }
    if ((asbd->mFormatFlags & kAudioFormatFlagIsBigEndian) != 0) {
        return false;
    }

    const bool is_float =
        (asbd->mFormatFlags & kAudioFormatFlagIsFloat) != 0 && asbd->mBitsPerChannel == 32;
    const bool is_int16 =
        (asbd->mFormatFlags & kAudioFormatFlagIsSignedInteger) != 0 && asbd->mBitsPerChannel == 16;
    if (!is_float && !is_int16) {
        return false;
    }

    size_t abl_size = 0;
    OSStatus status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample_buffer,
        &abl_size,
        NULL,
        0,
        NULL,
        NULL,
        0,
        NULL
    );
    if (status != noErr || abl_size < sizeof(AudioBufferList)) {
        return false;
    }

    uint8_t stack_abl[512];
    AudioBufferList *abl = (AudioBufferList *)stack_abl;
    bool heap_abl = false;
    if (abl_size > sizeof(stack_abl)) {
        abl = (AudioBufferList *)malloc(abl_size);
        if (!abl) {
            return false;
        }
        heap_abl = true;
    }

    CMBlockBufferRef block_buffer = NULL;
    status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample_buffer,
        &abl_size,
        abl,
        abl_size,
        kCFAllocatorDefault,
        kCFAllocatorDefault,
        kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment,
        &block_buffer
    );
    if (status != noErr || abl->mNumberBuffers == 0) {
        if (block_buffer) {
            CFRelease(block_buffer);
        }
        if (heap_abl) {
            free(abl);
        }
        return false;
    }

    const uint32_t channels = asbd->mChannelsPerFrame;
    const bool is_planar =
        (asbd->mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0 || abl->mNumberBuffers > 1;
    const size_t bytes_per_sample = is_float ? sizeof(float) : sizeof(int16_t);
    size_t in_frames = 0;
    if (is_planar) {
        in_frames = abl->mBuffers[0].mDataByteSize / bytes_per_sample;
        for (UInt32 b = 1; b < abl->mNumberBuffers; ++b) {
            const size_t ch_frames = abl->mBuffers[b].mDataByteSize / bytes_per_sample;
            if (ch_frames < in_frames) {
                in_frames = ch_frames;
            }
        }
    } else {
        in_frames = abl->mBuffers[0].mDataByteSize / (bytes_per_sample * (size_t)channels);
    }

    if (in_frames == 0) {
        if (block_buffer) {
            CFRelease(block_buffer);
        }
        if (heap_abl) {
            free(abl);
        }
        return true;
    }

    const bool same_rate = fabs(asbd->mSampleRate - (double)REMOTERS_AUDIO_SAMPLE_RATE) < 1.0;
    size_t out_frames = in_frames;
    if (!same_rate) {
        const long long scaled =
            llround((double)in_frames * (double)REMOTERS_AUDIO_SAMPLE_RATE / asbd->mSampleRate);
        out_frames = scaled > 0 ? (size_t)scaled : 1;
    }

    int16_t stack_pcm[4096];
    int16_t *out_pcm = stack_pcm;
    bool heap_pcm = false;
    const size_t out_samples = out_frames * REMOTERS_AUDIO_CHANNELS;
    if (out_samples > (sizeof(stack_pcm) / sizeof(stack_pcm[0]))) {
        out_pcm = (int16_t *)malloc(out_samples * sizeof(int16_t));
        if (!out_pcm) {
            if (block_buffer) {
                CFRelease(block_buffer);
            }
            if (heap_abl) {
                free(abl);
            }
            return false;
        }
        heap_pcm = true;
    }

    if (same_rate && !is_planar && is_int16 && channels == 2 && abl->mBuffers[0].mData) {
        memcpy(out_pcm, abl->mBuffers[0].mData, out_samples * sizeof(int16_t));
    } else if (same_rate) {
        const uint32_t right_ch = channels >= 2 ? 1 : 0;
        for (size_t f = 0; f < in_frames; ++f) {
            const float l = read_normalized_sample(abl, is_float, is_planar, channels, f, 0);
            const float r = read_normalized_sample(abl, is_float, is_planar, channels, f, right_ch);
            out_pcm[f * 2] = float_to_i16(l);
            out_pcm[f * 2 + 1] = float_to_i16(r);
        }
    } else {
        const uint32_t right_ch = channels >= 2 ? 1 : 0;
        const double ratio = asbd->mSampleRate / (double)REMOTERS_AUDIO_SAMPLE_RATE;
        for (size_t f = 0; f < out_frames; ++f) {
            const double pos = (double)f * ratio;
            size_t idx0 = (size_t)pos;
            if (idx0 >= in_frames) {
                idx0 = in_frames - 1;
            }
            const size_t idx1 = (idx0 + 1 < in_frames) ? (idx0 + 1) : idx0;
            const float frac = (float)(pos - (double)idx0);

            const float l0 = read_normalized_sample(abl, is_float, is_planar, channels, idx0, 0);
            const float l1 = read_normalized_sample(abl, is_float, is_planar, channels, idx1, 0);
            const float r0 =
                read_normalized_sample(abl, is_float, is_planar, channels, idx0, right_ch);
            const float r1 =
                read_normalized_sample(abl, is_float, is_planar, channels, idx1, right_ch);

            out_pcm[f * 2] = float_to_i16(l0 + (l1 - l0) * frac);
            out_pcm[f * 2 + 1] = float_to_i16(r0 + (r1 - r0) * frac);
        }
    }

    callback(ctx, out_pcm, out_samples);

    if (heap_pcm) {
        free(out_pcm);
    }
    if (block_buffer) {
        CFRelease(block_buffer);
    }
    if (heap_abl) {
        free(abl);
    }
    return true;
}
